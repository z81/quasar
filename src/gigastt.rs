//! Streaming speech recognition through a gigastt server (GigaAM v3,
//! https://github.com/ekhodzitsky/gigastt, WebSocket protocol of v2.18.0,
//! docs/api.md): while the station records, its OGG/Opus audio is decoded by
//! ffmpeg to 16 kHz PCM16 and streamed to `GET /v1/ws`. With
//! STT_END_OF_SPEECH=server gigastt also tells when the user stopped speaking
//! (`Recognition::next_endpoint`). At the end of the utterance the client
//! sends `stop` and receives the final transcript.
//!
//! The protocol as used here:
//! - the upgrade is refused with HTTP 503 while the model loads;
//! - the server sends `{"type":"ready", "supported_rates": [...], ...}` once an
//!   inference slot is free;
//! - the client sends `configure` before the first audio: `sample_rate` 16000
//!   and `endpoint_mode`, then binary PCM16 LE frames. `manual`: only `stop`
//!   ends the utterance. `auto` (STT_END_OF_SPEECH=server): with `--vad` the
//!   server's Silero VAD ends an utterance after `min_silence_ms` of silence
//!   following speech (sent per session); without it, the decoder does after
//!   a ~0.6 s run of blank frames, checked every 0.8 s of audio;
//! - the server streams `partial` (raw hypothesis) and `final` messages (a
//!   final closes an utterance and carries all of its words, timed in seconds
//!   of the stream; an automatic end has `endpoint_reason` `vad` or `blank`)
//!   and `error` messages (`code`, `message`). After an automatic end the
//!   session goes on with the next utterance;
//! - `{"type":"stop"}` makes the server decode what is left, send one last
//!   `final` with `endpoint_reason: "stop"` (possibly empty) and end the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::audio::Transcoder;
use crate::config::{Config, EndOfSpeech, SttProvider};

/// The PCM rate sent to gigastt: GigaAM's own, so the server resamples nothing
const SAMPLE_RATE: u32 = 16_000;

/// PCM16 mono at SAMPLE_RATE
const BYTES_PER_SECOND: u64 = SAMPLE_RATE as u64 * 2;

/// Setting up a session: the handshake and the server's `ready`, which waits
/// for a free inference slot
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Up to 200 ms of PCM per binary frame
const FRAME_BYTES: usize = 6_400;

/// Closing a session properly (the server logs a dropped connection as an error)
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// The blank run after which gigastt's decoder (no VAD) ends an utterance
const BLANK_RUN: Duration = Duration::from_millis(600);

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct Gigastt {
    ws_url: String,
    /// None: manual endpointing, the station's Close ends the utterance
    endpointing: Option<Endpointing>,
}

/// gigastt's automatic endpointing (STT_END_OF_SPEECH=server)
#[derive(Clone, Copy)]
struct Endpointing {
    /// the trailing silence that ends an utterance, for the server's VAD
    min_silence_ms: u32,
}

impl Gigastt {
    /// The recognizer of STT_PROVIDER=gigaam; a misconfiguration stops the server at start
    pub fn from_config(config: &Config) -> Option<Self> {
        if config.stt_provider != SttProvider::GigaAm {
            return None;
        }
        let base_url = config.gigastt_url.as_deref().expect("GIGASTT_URL is required with STT_PROVIDER=gigaam");
        let endpointing = (config.end_of_speech == EndOfSpeech::Server)
            .then_some(Endpointing { min_silence_ms: config.gigastt_min_silence_ms });
        let gigastt = Self::new(base_url, endpointing).unwrap_or_else(|error| panic!("{error:#}"));
        match endpointing {
            Some(Endpointing { min_silence_ms }) => tracing::info!(
                "speech recognition: gigastt at {} (OpenRouter as fallback); gigastt ends utterances: \
                 {min_silence_ms} ms of silence if it runs with --vad, a ~0.6 s blank run of its decoder otherwise",
                gigastt.ws_url
            ),
            None => tracing::info!(
                "speech recognition: gigastt at {} (OpenRouter as fallback); the station ends utterances",
                gigastt.ws_url
            ),
        }
        Some(gigastt)
    }

    /// `base_url` is the server's HTTP address, e.g. http://gigastt:9876
    fn new(base_url: &str, endpointing: Option<Endpointing>) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        let ws_url = match base_url.split_once("://") {
            Some(("http" | "ws", rest)) => format!("ws://{rest}/v1/ws"),
            // no TLS client: gigastt runs next to this server, on a private network
            _ => bail!("GIGASTT_URL must be a plain http:// address, not '{base_url}'"),
        };
        Ok(Self { ws_url, endpointing })
    }

    /// Starts recognizing one voice input; dropping the handle stops it
    pub fn start(&self) -> Recognition {
        let (audio, audio_receiver) = mpsc::unbounded_channel();
        let (result, result_receiver) = oneshot::channel();
        let (cancel, cancelled) = oneshot::channel();
        let (endpoints, endpoint_receiver) = match self.endpointing {
            Some(_) => {
                let (sender, receiver) = mpsc::unbounded_channel();
                (Some(sender), Some(receiver))
            }
            None => (None, None),
        };
        let gigastt = self.clone();
        // ends on its own: at the transcript, a failure, or a cancellation
        // (then within CLOSE_TIMEOUT)
        tokio::spawn(async move {
            let outcome = gigastt.session(audio_receiver, endpoints, cancelled).await;
            if let Err(error) = &outcome {
                tracing::debug!("gigastt session ended: {error:#}");
            }
            let _ = result.send(outcome);
        });
        Recognition { audio: Some(audio), endpoints: endpoint_receiver, result: result_receiver, _cancel: cancel }
    }
}

/// What gigastt recognized in a voice input
pub struct Transcript {
    /// the finals of the session, joined
    pub text: String,
    /// when the latest recognized word ended, from its time in the audio
    pub speech_ended_at: Option<Instant>,
}

/// An end of utterance gigastt detected while the station still records
pub struct Endpoint {
    /// the finals so far, joined: the utterance as recognized up to here
    pub text: String,
    /// `vad` (silence after speech) or `blank` (the decoder's blank run)
    pub reason: String,
    /// the audio streamed so far
    pub audio: Duration,
    /// where the speech lies in that audio: the first word's start, the latest word's end
    pub speech: Option<(Duration, Duration)>,
    /// when the latest word ended
    pub speech_ended_at: Option<Instant>,
}

impl Endpoint {
    /// Whether the user spoke on after the pause this end was found at.
    /// Without a VAD, gigastt 2.18.0 ends an utterance once a decoded window
    /// holds a 0.6 s blank run anywhere after a word, so a pause the user has
    /// already spoken past ends it too (with the words said since); a true
    /// end has at least that much silence after the latest word
    pub fn spoken_past(&self) -> bool {
        self.reason == "blank" && self.speech.is_some_and(|(_, end)| self.audio.saturating_sub(end) < BLANK_RUN)
    }

    /// The audio and the speech in it, for the logs
    pub fn timing(&self) -> String {
        let audio = self.audio.as_secs_f64();
        match self.speech {
            Some((start, end)) => format!(
                "audio {audio:.2} s, speech {:.2}-{:.2} s: {:.2} s since it started, {:.2} s of silence",
                start.as_secs_f64(),
                end.as_secs_f64(),
                self.audio.saturating_sub(start).as_secs_f64(),
                self.audio.saturating_sub(end).as_secs_f64()
            ),
            None => format!("audio {audio:.2} s"),
        }
    }
}

/// One voice input being recognized: the station's audio goes in, the
/// transcript comes out. Dropping it (a cancelled or replaced voice input, a
/// closed connection) kills the decoder and closes the session
pub struct Recognition {
    audio: Option<mpsc::UnboundedSender<Bytes>>,
    /// gigastt's ends of utterance (automatic endpointing only)
    endpoints: Option<mpsc::UnboundedReceiver<Endpoint>>,
    result: oneshot::Receiver<Result<Transcript>>,
    /// Dropped with the handle, which cancels the session
    _cancel: oneshot::Sender<()>,
}

impl Recognition {
    /// The station's OGG/Opus audio as it arrives; after a failure it is
    /// dropped, the failure waits for `finish`
    pub fn push_audio(&self, ogg: &[u8]) {
        if let Some(audio) = &self.audio {
            let _ = audio.send(Bytes::copy_from_slice(ogg));
        }
    }

    /// The next end of utterance gigastt detected; None with manual
    /// endpointing or once the session is over
    pub async fn next_endpoint(&mut self) -> Option<Endpoint> {
        self.endpoints.as_mut()?.recv().await
    }

    /// The end of the utterance: the decoder flushes, `stop` goes out and the
    /// final transcript is awaited for at most `wait`
    pub async fn finish(mut self, wait: Duration) -> Result<Transcript> {
        self.audio = None;
        self.endpoints = None;
        match tokio::time::timeout(wait, &mut self.result).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(anyhow!("the session ended without a result")),
            Err(_) => Err(anyhow!("no final transcript within {wait:?}")),
        }
    }
}

impl Gigastt {
    async fn session(
        &self,
        audio: mpsc::UnboundedReceiver<Bytes>,
        endpoints: Option<mpsc::UnboundedSender<Endpoint>>,
        cancelled: oneshot::Receiver<()>,
    ) -> Result<Transcript> {
        // the decoder starts at once: the speech is decoded while the session is set up
        let (decoder, ogg_input, pcm_output) = Transcoder::ogg_to_pcm()?;
        let mut socket = None;
        let outcome = tokio::select! {
            outcome = self.recognize(audio, ogg_input, pcm_output, endpoints, &mut socket) => outcome,
            _ = cancelled => Err(anyhow!("cancelled")),
        };
        let outcome = match outcome {
            Ok(transcript) => decoder.finish().await.map(|()| transcript),
            // dropping the decoder kills ffmpeg
            Err(error) => Err(error),
        };
        if let Some(mut socket) = socket {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.close(None)).await;
        }
        outcome
    }

    async fn recognize(
        &self,
        mut audio: mpsc::UnboundedReceiver<Bytes>,
        mut ogg_input: ChildStdin,
        mut pcm_output: ChildStdout,
        endpoints: Option<mpsc::UnboundedSender<Endpoint>>,
        socket: &mut Option<Socket>,
    ) -> Result<Transcript> {
        let feed = async {
            while let Some(ogg) = audio.recv().await {
                ogg_input.write_all(&ogg).await.context("decoder input")?;
            }
            // the end of the utterance: the decoder flushes the rest and ends its output
            drop(ogg_input);
            Ok(())
        };
        let transcribe = async {
            let socket = socket.insert(self.connect().await?);
            stream_session(socket, &mut pcm_output, endpoints.as_ref()).await
        };
        let ((), transcript) = tokio::try_join!(feed, transcribe)?;
        Ok(transcript)
    }

    /// The handshake, the server's `ready` and the session's configuration
    async fn connect(&self) -> Result<Socket> {
        tokio::time::timeout(CONNECT_TIMEOUT, self.open_session())
            .await
            .map_err(|_| anyhow!("not ready within {CONNECT_TIMEOUT:?}"))?
    }

    async fn open_session(&self) -> Result<Socket> {
        let (mut socket, _) = tokio_tungstenite::connect_async(&self.ws_url).await.map_err(|error| match error {
            // 503 while the model loads (`initializing`) or the server shuts down
            tungstenite::Error::Http(response) => {
                let code = response
                    .body()
                    .as_deref()
                    .and_then(|body| serde_json::from_slice::<Value>(body).ok())
                    .and_then(|body| body["code"].as_str().map(str::to_string))
                    .unwrap_or_default();
                anyhow!("not ready: HTTP {} {code}", response.status())
            }
            other => anyhow!("unreachable: {other}"),
        })?;
        let ready = match next_message(&mut socket).await? {
            Some(message) => message,
            None => bail!("closed before ready"),
        };
        match ready["type"].as_str() {
            Some("ready") => {}
            // e.g. `timeout` when no inference slot freed up
            Some("error") => bail!("error {} instead of ready: {}", ready["code"], ready["message"]),
            other => bail!("{other:?} instead of ready"),
        }
        if let Some(rates) = ready["supported_rates"].as_array() {
            if !rates.iter().any(|rate| rate.as_u64() == Some(u64::from(SAMPLE_RATE))) {
                bail!("{SAMPLE_RATE} Hz is not among the supported rates {rates:?}");
            }
        }
        socket.send(Message::Text(self.configure().to_string().into())).await?;
        Ok(socket)
    }

    /// The session's settings, sent before the first audio
    fn configure(&self) -> Value {
        match self.endpointing {
            Some(Endpointing { min_silence_ms }) => json!({
                "type": "configure",
                "sample_rate": SAMPLE_RATE,
                "endpoint_mode": "auto",
                "min_silence_ms": min_silence_ms,
            }),
            None => json!({ "type": "configure", "sample_rate": SAMPLE_RATE, "endpoint_mode": "manual" }),
        }
    }
}

/// The next JSON message of the server; None once it closed the session
async fn next_message(socket: &mut Socket) -> Result<Option<Value>> {
    while let Some(message) = socket.next().await {
        match message.context("connection failed")? {
            Message::Text(text) => return Ok(Some(serde_json::from_str(text.as_str()).context("invalid message")?)),
            Message::Close(_) => return Ok(None),
            // pings are answered by the library
            _ => {}
        }
    }
    Ok(None)
}

/// Streams the decoder's PCM until it ends, then `stop`; meanwhile collects
/// the finals up to the one answering `stop` and reports the automatic ends
/// of utterance before it
async fn stream_session(
    socket: &mut Socket,
    pcm: &mut ChildStdout,
    endpoints: Option<&mpsc::UnboundedSender<Endpoint>>,
) -> Result<Transcript> {
    let (mut sink, mut stream) = socket.split();
    let stopped = AtomicBool::new(false);
    let clock = Mutex::new(PcmClock::default());
    let send = async {
        let mut buffer = vec![0; FRAME_BYTES];
        loop {
            let read = pcm.read(&mut buffer).await.context("decoder output")?;
            if read == 0 {
                break;
            }
            sink.send(Message::Binary(buffer[..read].to_vec().into())).await?;
            clock.lock().unwrap_or_else(PoisonError::into_inner).advance(read);
        }
        sink.send(Message::Text(json!({ "type": "stop" }).to_string().into())).await?;
        stopped.store(true, Ordering::Relaxed);
        Ok(())
    };
    let receive = async {
        let mut finals = Finals::default();
        while let Some(message) = stream.next().await {
            let text = match message.context("connection failed")? {
                Message::Text(text) => text,
                Message::Close(frame) => bail!("closed early: {frame:?}"),
                _ => continue,
            };
            let message: Value = serde_json::from_str(text.as_str()).context("invalid message")?;
            match message["type"].as_str() {
                Some("partial") => tracing::debug!("gigastt partial: {}", message["text"]),
                Some("final") => {
                    let clock = *clock.lock().unwrap_or_else(PoisonError::into_inner);
                    finals.add(&message, &clock);
                    match message["endpoint_reason"].as_str() {
                        Some("stop") => return Ok(finals.transcript()),
                        // an automatic end of utterance, while the audio still flows
                        reason => {
                            if let Some(endpoints) = endpoints.filter(|_| !stopped.load(Ordering::Relaxed)) {
                                let _ = endpoints.send(finals.endpoint(reason.unwrap_or("unnamed"), &clock));
                            }
                        }
                    }
                }
                Some("error") => bail!("error {}: {}", message["code"], message["message"]),
                // types added by later protocol versions
                _ => {}
            }
        }
        // a server that ends the session after `stop` without naming the reason
        if stopped.load(Ordering::Relaxed) && !finals.texts.is_empty() {
            return Ok(finals.transcript());
        }
        bail!("closed before the final transcript")
    };
    let ((), transcript) = tokio::try_join!(send, receive)?;
    Ok(transcript)
}

/// How much PCM went to gigastt, and when its latest frame did
#[derive(Clone, Copy, Default)]
struct PcmClock {
    bytes: u64,
    sent_at: Option<Instant>,
}

impl PcmClock {
    fn advance(&mut self, bytes: usize) {
        self.bytes += bytes as u64;
        self.sent_at = Some(Instant::now());
    }

    /// The audio streamed so far
    fn audio(&self) -> Duration {
        Duration::from_micros(self.bytes * 1_000_000 / BYTES_PER_SECOND)
    }

    /// When the audio at `position` was heard: the station streams as it
    /// records, so the latest frame stands for "now" in the stream's time
    fn instant_of(&self, position: Duration) -> Option<Instant> {
        self.sent_at?.checked_sub(self.audio().saturating_sub(position))
    }
}

/// The finals of a session and where its speech lies in the audio
#[derive(Default)]
struct Finals {
    texts: Vec<String>,
    /// the first word's start and the latest word's end, in seconds of the stream
    speech_start: Option<Duration>,
    speech_end: Option<Duration>,
    speech_ended_at: Option<Instant>,
}

impl Finals {
    fn add(&mut self, message: &Value, clock: &PcmClock) {
        let text = message["text"].as_str().unwrap_or_default().trim();
        // an empty final, or one of punctuation alone, adds nothing
        if text.chars().any(char::is_alphanumeric) {
            self.texts.push(text.to_string());
        }
        let words = message["words"].as_array().map(Vec::as_slice).unwrap_or_default();
        let time = |word: Option<&Value>, edge: &str| {
            word.and_then(|word| word[edge].as_f64()).and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        };
        if self.speech_start.is_none() {
            self.speech_start = time(words.first(), "start");
        }
        if let Some(end) = time(words.last(), "end") {
            self.speech_end = Some(end);
            self.speech_ended_at = clock.instant_of(end);
        }
    }

    fn transcript(&self) -> Transcript {
        Transcript { text: self.texts.join(" "), speech_ended_at: self.speech_ended_at }
    }

    fn endpoint(&self, reason: &str, clock: &PcmClock) -> Endpoint {
        Endpoint {
            text: self.texts.join(" "),
            reason: reason.to_string(),
            audio: clock.audio(),
            speech: self.speech_start.zip(self.speech_end),
            speech_ended_at: self.speech_ended_at,
        }
    }
}
