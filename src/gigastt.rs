//! Streaming speech recognition through a gigastt server (GigaAM v3,
//! https://github.com/ekhodzitsky/gigastt, WebSocket protocol of v2.18.0,
//! docs/api.md): while the station records, its OGG/Opus audio is decoded by
//! ffmpeg to 16 kHz PCM16 and streamed to `GET /v1/ws`; at the end of speech
//! the client sends `stop` and receives the final transcript.
//!
//! The protocol as used here:
//! - the upgrade is refused with HTTP 503 while the model loads;
//! - the server sends `{"type":"ready", "supported_rates": [...], ...}` once an
//!   inference slot is free;
//! - the client sends `{"type":"configure", "sample_rate":16000,
//!   "endpoint_mode":"manual"}` before the first audio (manual: the station,
//!   not the server's VAD, ends the utterance), then binary PCM16 LE frames;
//! - the server streams `partial` (raw hypothesis) and `final` messages (a
//!   final closes an utterance and carries all of its words) and `error`
//!   messages (`code`, `message`);
//! - `{"type":"stop"}` makes the server decode what is left, send one last
//!   `final` with `endpoint_reason: "stop"` (possibly empty) and end the session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::audio::Transcoder;
use crate::config::{Config, SttProvider};

/// The PCM rate sent to gigastt: GigaAM's own, so the server resamples nothing
const SAMPLE_RATE: u32 = 16_000;

/// Setting up a session: the handshake and the server's `ready`, which waits
/// for a free inference slot
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Up to 200 ms of PCM per binary frame
const FRAME_BYTES: usize = 6_400;

/// Closing a session properly (the server logs a dropped connection as an error)
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct Gigastt {
    ws_url: String,
}

impl Gigastt {
    /// The recognizer of STT_PROVIDER=gigaam; a misconfiguration stops the server at start
    pub fn from_config(config: &Config) -> Option<Self> {
        if config.stt_provider != SttProvider::GigaAm {
            return None;
        }
        let base_url = config.gigastt_url.as_deref().expect("GIGASTT_URL is required with STT_PROVIDER=gigaam");
        let gigastt = Self::new(base_url).unwrap_or_else(|error| panic!("{error:#}"));
        tracing::info!("speech recognition: gigastt at {} (OpenRouter as fallback)", gigastt.ws_url);
        Some(gigastt)
    }

    /// `base_url` is the server's HTTP address, e.g. http://gigastt:9876
    fn new(base_url: &str) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        let ws_url = match base_url.split_once("://") {
            Some(("http" | "ws", rest)) => format!("ws://{rest}/v1/ws"),
            // no TLS client: gigastt runs next to this server, on a private network
            _ => bail!("GIGASTT_URL must be a plain http:// address, not '{base_url}'"),
        };
        Ok(Self { ws_url })
    }

    /// Starts recognizing one voice input; dropping the handle stops it
    pub fn start(&self) -> Recognition {
        let (audio, audio_receiver) = mpsc::unbounded_channel();
        let (result, result_receiver) = oneshot::channel();
        let (cancel, cancelled) = oneshot::channel();
        let ws_url = self.ws_url.clone();
        // ends on its own: at the transcript, a failure, or a cancellation
        // (then within CLOSE_TIMEOUT)
        tokio::spawn(async move {
            let outcome = session(&ws_url, audio_receiver, cancelled).await;
            if let Err(error) = &outcome {
                tracing::debug!("gigastt session ended: {error:#}");
            }
            let _ = result.send(outcome);
        });
        Recognition { audio: Some(audio), result: result_receiver, _cancel: cancel }
    }
}

/// One voice input being recognized: the station's audio goes in, the
/// transcript comes out. Dropping it (a cancelled or replaced voice input, a
/// closed connection) kills the decoder and closes the session
pub struct Recognition {
    audio: Option<mpsc::UnboundedSender<Bytes>>,
    result: oneshot::Receiver<Result<String>>,
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

    /// The end of speech: the decoder flushes, `stop` goes out and the final
    /// transcript is awaited for at most `wait`
    pub async fn finish(mut self, wait: Duration) -> Result<String> {
        self.audio = None;
        match tokio::time::timeout(wait, &mut self.result).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(anyhow!("the session ended without a result")),
            Err(_) => Err(anyhow!("no final transcript within {wait:?}")),
        }
    }
}

async fn session(
    ws_url: &str,
    audio: mpsc::UnboundedReceiver<Bytes>,
    cancelled: oneshot::Receiver<()>,
) -> Result<String> {
    // the decoder starts at once: the speech is decoded while the session is set up
    let (decoder, ogg_input, pcm_output) = Transcoder::ogg_to_pcm()?;
    let mut socket = None;
    let outcome = tokio::select! {
        outcome = recognize(ws_url, audio, ogg_input, pcm_output, &mut socket) => outcome,
        _ = cancelled => Err(anyhow!("cancelled")),
    };
    let outcome = match outcome {
        Ok(text) => decoder.finish().await.map(|()| text),
        // dropping the decoder kills ffmpeg
        Err(error) => Err(error),
    };
    if let Some(mut socket) = socket {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.close(None)).await;
    }
    outcome
}

async fn recognize(
    ws_url: &str,
    mut audio: mpsc::UnboundedReceiver<Bytes>,
    mut ogg_input: ChildStdin,
    mut pcm_output: ChildStdout,
    socket: &mut Option<Socket>,
) -> Result<String> {
    let feed = async {
        while let Some(ogg) = audio.recv().await {
            ogg_input.write_all(&ogg).await.context("decoder input")?;
        }
        // the end of speech: the decoder flushes the rest and ends its output
        drop(ogg_input);
        Ok(())
    };
    let transcribe = async {
        let socket = socket.insert(connect(ws_url).await?);
        stream_session(socket, &mut pcm_output).await
    };
    let ((), text) = tokio::try_join!(feed, transcribe)?;
    Ok(text)
}

/// The handshake, the server's `ready` and the session's configuration
async fn connect(ws_url: &str) -> Result<Socket> {
    let session = async {
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.map_err(|error| match error {
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
        let configure = json!({ "type": "configure", "sample_rate": SAMPLE_RATE, "endpoint_mode": "manual" });
        socket.send(Message::Text(configure.to_string().into())).await?;
        Ok(socket)
    };
    tokio::time::timeout(CONNECT_TIMEOUT, session).await.map_err(|_| anyhow!("not ready within {CONNECT_TIMEOUT:?}"))?
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
/// the finals up to the one answering `stop`
async fn stream_session(socket: &mut Socket, pcm: &mut ChildStdout) -> Result<String> {
    let (mut sink, mut stream) = socket.split();
    let stopped = AtomicBool::new(false);
    let send = async {
        let mut buffer = vec![0; FRAME_BYTES];
        loop {
            let read = pcm.read(&mut buffer).await.context("decoder output")?;
            if read == 0 {
                break;
            }
            sink.send(Message::Binary(buffer[..read].to_vec().into())).await?;
        }
        sink.send(Message::Text(json!({ "type": "stop" }).to_string().into())).await?;
        stopped.store(true, Ordering::Relaxed);
        Ok(())
    };
    let receive = async {
        let mut finals = Vec::new();
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
                    let words = message["text"].as_str().unwrap_or_default().trim();
                    if !words.is_empty() {
                        finals.push(words.to_string());
                    }
                    if message["endpoint_reason"] == "stop" {
                        return Ok(finals.join(" "));
                    }
                }
                Some("error") => bail!("error {}: {}", message["code"], message["message"]),
                // types added by later protocol versions
                _ => {}
            }
        }
        // a server that ends the session after `stop` without naming the reason
        if stopped.load(Ordering::Relaxed) && !finals.is_empty() {
            return Ok(finals.join(" "));
        }
        bail!("closed before the final transcript")
    };
    let ((), text) = tokio::try_join!(send, receive)?;
    Ok(text)
}
