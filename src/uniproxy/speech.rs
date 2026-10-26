//! The speech of one answer part: its text chunks are synthesized in order,
//! the next ones while the current one is still being encoded, and their PCM
//! goes through one ffmpeg encoder whose OGG/Opus pages make the part's single
//! LazyTtsStreaming stream (one TtsSpeak, one closing StreamControl). Speech
//! that is not raw PCM at the encoder's rate — MP3 (OPENROUTER_TTS_FORMAT=mp3),
//! Piper's WAV (TTS_PROVIDER=piper) — is decoded chunk by chunk, each by its
//! own ffmpeg, to PCM at one rate whatever the provider's. A chunk Piper
//! fails on is spoken by OpenRouter instead (PIPER_FALLBACK).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::UniproxyContext;
use super::outbox::Outbox;
use crate::audio::{Encoded, Transcoder};
use crate::config::TtsFormat;
use crate::piper::Piper;

/// Chunks synthesized ahead of the one being encoded, so the next words are
/// ready by the time the current ones end
const PREFETCH: usize = 2;

/// Upper bound for one part's speech: a stalled synthesis must not keep the
/// stream and ffmpeg alive forever
const SPEECH_LIFETIME: Duration = Duration::from_secs(120);

/// The rate decoded speech (MP3, Piper's WAV) is brought to, whatever rate it was encoded at
const SPEECH_RATE: u32 = 24_000;

/// At most this much of a chunk's text goes into its log line
const LOGGED_TEXT_CHARS: usize = 40;

/// The PCM of one chunk as it arrives
type Pcm = mpsc::UnboundedReceiver<Result<Bytes>>;

/// When the voice request being answered ended: the answer's first audio
/// frame logs its latency from there
#[derive(Clone, Copy)]
pub struct SpeechEnd {
    /// the end of the last recognized word (gigastt's word times), if known
    pub speech: Option<Instant>,
    /// the end of the utterance: gigastt's end of utterance or the station's Close
    pub utterance: Instant,
}

impl SpeechEnd {
    fn log_first_audio(self) {
        let utterance = self.utterance.elapsed().as_millis();
        match self.speech {
            Some(speech) => tracing::info!(
                "first answer audio sent {} ms after the end of speech ({utterance} ms after the end of the utterance)",
                speech.elapsed().as_millis()
            ),
            None => tracing::info!("first answer audio sent {utterance} ms after the end of the utterance"),
        }
    }
}

/// Speaks a text known whole; empty text is not spoken at all
pub async fn speak_text(
    context: &Arc<UniproxyContext>,
    outbox: &Outbox,
    text: &str,
    reference_message_id: &str,
    speech_end: Option<SpeechEnd>,
) {
    if text.is_empty() {
        return;
    }
    let (chunks, receiver) = mpsc::unbounded_channel();
    let _ = chunks.send(text.to_string());
    drop(chunks);
    speak(context, outbox, receiver, reference_message_id, speech_end).await;
}

/// Speaks the chunks of a part through one TTS stream, closed once the
/// chunks end; on a failure, an abort or a timeout the stream is closed too
pub async fn speak(
    context: &Arc<UniproxyContext>,
    outbox: &Outbox,
    chunks: mpsc::UnboundedReceiver<String>,
    reference_message_id: &str,
    speech_end: Option<SpeechEnd>,
) {
    let speech = stream_speech(context, outbox, chunks, reference_message_id, speech_end);
    match tokio::time::timeout(SPEECH_LIFETIME, speech).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("TTS streaming failed: {error:#}"),
        Err(_) => tracing::error!("TTS streaming took longer than {SPEECH_LIFETIME:?}"),
    }
}

async fn stream_speech(
    context: &Arc<UniproxyContext>,
    outbox: &Outbox,
    mut chunks: mpsc::UnboundedReceiver<String>,
    reference_message_id: &str,
    mut speech_end: Option<SpeechEnd>,
) -> Result<()> {
    // dropping the guard closes the stream, dropping the encoder kills ffmpeg
    let stream = outbox.open_tts_stream(reference_message_id)?;
    let (encoder, input, mut output) = Transcoder::pcm_to_ogg(pcm_rate(context))?;
    let drain = async {
        let mut buffer = vec![0; 16_384];
        loop {
            let read = output.read(&mut buffer).await.context("reading OGG from ffmpeg")?;
            if read == 0 {
                return Ok(());
            }
            stream.send_audio(&buffer[..read])?;
            if let Some(speech_end) = speech_end.take() {
                speech_end.log_first_audio();
            }
        }
    };
    tokio::try_join!(feed(context, &mut chunks, input), drain)?;
    encoder.finish().await
}

/// Writes the chunks' PCM into ffmpeg in their order, synthesizing up to
/// PREFETCH chunks ahead of the one being written
async fn feed(
    context: &Arc<UniproxyContext>,
    chunks: &mut mpsc::UnboundedReceiver<String>,
    mut input: ChildStdin,
) -> Result<()> {
    // dropping the set (on an abort) cancels the requests still running
    let mut syntheses = JoinSet::new();
    let mut waiting = VecDeque::new();
    let mut started: VecDeque<Pcm> = VecDeque::new();
    let mut more_text = true;
    let mut odd_byte = None;
    loop {
        while started.len() <= PREFETCH {
            let Some(text) = waiting.pop_front() else {
                break;
            };
            started.push_back(synthesize(context, text, &mut syntheses));
        }
        if started.is_empty() && !more_text {
            break;
        }
        tokio::select! {
            text = chunks.recv(), if more_text => match text {
                Some(text) if !text.trim().is_empty() => waiting.push_back(text.trim().to_string()),
                Some(_) => {}
                None => more_text = false,
            },
            pcm = next_pcm(&mut started), if !started.is_empty() => match pcm {
                Some(Ok(pcm)) => write_samples(&mut input, &mut odd_byte, &pcm).await.context("writing PCM to ffmpeg")?,
                Some(Err(error)) => {
                    tracing::warn!("a chunk is skipped: {error:#}");
                    started.pop_front();
                    odd_byte = None;
                }
                // the chunk is complete; a lone byte of a broken sample is dropped
                None => {
                    started.pop_front();
                    odd_byte = None;
                }
            },
        }
    }
    // the end of the PCM: ffmpeg writes the last pages and exits
    drop(input);
    Ok(())
}

/// The rate of the PCM the part's encoder takes: OpenRouter's raw PCM as it
/// comes, anything decoded at SPEECH_RATE
fn pcm_rate(context: &UniproxyContext) -> u32 {
    match (&context.piper, context.config.tts_format) {
        (None, TtsFormat::Pcm) => context.config.tts_pcm_sample_rate,
        _ => SPEECH_RATE,
    }
}

/// Requests the synthesis of a chunk; its PCM comes through the returned
/// channel as it arrives. Dropping the set aborts the task, which kills its
/// decoder
fn synthesize(context: &Arc<UniproxyContext>, text: String, syntheses: &mut JoinSet<()>) -> Pcm {
    let (pcm, receiver) = mpsc::unbounded_channel();
    let context = context.clone();
    syntheses.spawn(async move {
        tracing::debug!("synthesizing '{text}'");
        let started = Instant::now();
        let synthesized = match &context.piper {
            Some(piper) => synthesize_piper(&context, piper, &text, &pcm, started).await,
            None => synthesize_openrouter(&context, &text, &pcm, started).await,
        };
        match synthesized {
            Ok(synthesis) => synthesis.log(&text, pcm_rate(&context)),
            Err(error) => {
                let failed = format!("synthesis failed after {} ms", started.elapsed().as_millis());
                let _ = pcm.send(Err(error.context(failed)));
            }
        }
    });
    receiver
}

/// The chunk by Piper, its WAV decoded by its own ffmpeg. A chunk Piper
/// fails on before any of it was heard is spoken by OpenRouter instead
/// (PIPER_FALLBACK=openrouter), so the station never falls silent
async fn synthesize_piper(
    context: &UniproxyContext,
    piper: &Piper,
    text: &str,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
) -> Result<Synthesis> {
    let heard = AtomicBool::new(false);
    let attempt = async {
        // starts while the request is on its way; dropping it kills ffmpeg
        let decoder = Transcoder::decode_to_pcm(Encoded::Wav, SPEECH_RATE)?;
        let wav = piper.synthesize(text).await?;
        decode(decoder, wav, pcm, started, &heard).await
    };
    match attempt.await {
        Ok(synthesis) => Ok(synthesis.labelled("piper".to_string())),
        Err(error) if piper.falls_back() && !heard.load(Ordering::Relaxed) => {
            tracing::warn!(
                "Piper failed on '{}' after {} ms ({error:#}): OpenRouter speaks it instead",
                shown(text),
                started.elapsed().as_millis()
            );
            let synthesis = synthesize_openrouter(context, text, pcm, Instant::now()).await?;
            let label = match synthesis.label.as_str() {
                "" => "OpenRouter instead of Piper".to_string(),
                label => format!("OpenRouter instead of Piper, {label}"),
            };
            Ok(synthesis.labelled(label))
        }
        Err(error) => Err(error),
    }
}

/// The chunk by OpenRouter: raw PCM at the encoder's rate forwarded as it
/// comes, MP3 or PCM at another rate decoded by its own ffmpeg
async fn synthesize_openrouter(
    context: &UniproxyContext,
    text: &str,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
) -> Result<Synthesis> {
    let config = &context.config;
    let rate = pcm_rate(context);
    let encoded = match config.tts_format {
        TtsFormat::Mp3 => Some(Encoded::Mp3),
        TtsFormat::Pcm if config.tts_pcm_sample_rate == rate => None,
        TtsFormat::Pcm => Some(Encoded::Pcm(config.tts_pcm_sample_rate)),
    };
    // a decoder starts while the request is on its way; dropping it kills ffmpeg
    let decoder = encoded.map(|encoded| Transcoder::decode_to_pcm(encoded, rate)).transpose()?;
    let speech = context
        .openrouter
        .speech_stream(&config.tts_model, &config.tts_voice, &config.tts_instructions, config.tts_speed, config.tts_format, text)
        .await?;
    let Some(decoder) = decoder else {
        return forward(speech, pcm, started).await;
    };
    let label = match config.tts_format {
        TtsFormat::Mp3 => "mp3",
        TtsFormat::Pcm => "pcm, resampled",
    };
    Ok(decode(decoder, speech, pcm, started, &AtomicBool::new(false)).await?.labelled(label.to_string()))
}

/// Raw PCM at the encoder's rate, forwarded as it arrives
async fn forward(
    speech: impl Stream<Item = Result<Bytes>>,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
) -> Result<Synthesis> {
    tokio::pin!(speech);
    let mut synthesis = Synthesis::new(started, false);
    while let Some(piece) = speech.next().await {
        let piece = piece?;
        synthesis.received.add(started, piece.len());
        synthesis.decoded.add(started, piece.len());
        if pcm.send(Ok(piece)).is_err() {
            break;
        }
    }
    Ok(synthesis)
}

/// Encoded speech decoded by its own ffmpeg as it arrives: no bytes of two
/// answers ever meet in one decoder, so their headers, frames, encoder
/// delays and rates stay apart. `heard` turns true with the first PCM sent on
async fn decode(
    (decoder, mut input, mut output): (Transcoder, ChildStdin, ChildStdout),
    speech: impl Stream<Item = Result<Bytes>>,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
    heard: &AtomicBool,
) -> Result<Synthesis> {
    let feed = async {
        tokio::pin!(speech);
        let mut received = Flow::default();
        while let Some(piece) = speech.next().await {
            let piece = piece?;
            received.add(started, piece.len());
            input.write_all(&piece).await.context("writing speech to ffmpeg")?;
        }
        // the end of the speech: the decoder flushes and exits
        drop(input);
        Ok::<_, anyhow::Error>(received)
    };
    let drain = async {
        let mut buffer = vec![0; 16_384];
        let mut decoded = Flow::default();
        loop {
            let read = output.read(&mut buffer).await.context("reading PCM from ffmpeg")?;
            if read == 0 {
                return Ok::<_, anyhow::Error>(decoded);
            }
            decoded.add(started, read);
            pcm.send(Ok(Bytes::copy_from_slice(&buffer[..read]))).map_err(|_| anyhow!("the speech is over"))?;
            heard.store(true, Ordering::Relaxed);
        }
    };
    let (received, decoded) = tokio::try_join!(feed, drain)?;
    decoder.finish().await?;
    Ok(Synthesis { started, received, decoded, decoded_apart: true, label: String::new() })
}

/// What one chunk's synthesis took
struct Synthesis {
    started: Instant,
    /// the provider's answer
    received: Flow,
    /// the PCM it made
    decoded: Flow,
    /// decoded by its own ffmpeg, not forwarded as it came
    decoded_apart: bool,
    /// who spoke it in which form, for the log
    label: String,
}

/// Bytes as they came, and when the first of them did
#[derive(Default)]
struct Flow {
    first: Option<Duration>,
    bytes: usize,
}

impl Flow {
    fn add(&mut self, started: Instant, bytes: usize) {
        self.first.get_or_insert_with(|| started.elapsed());
        self.bytes += bytes;
    }
}

impl Synthesis {
    fn new(started: Instant, decoded_apart: bool) -> Self {
        Self { started, received: Flow::default(), decoded: Flow::default(), decoded_apart, label: String::new() }
    }

    fn labelled(self, label: String) -> Self {
        Self { label, ..self }
    }

    fn log(&self, text: &str, rate: u32) {
        let ms = |duration: Option<Duration>| duration.map_or_else(|| "-".to_string(), |duration| duration.as_millis().to_string());
        let label = if self.label.is_empty() { String::new() } else { format!(" ({})", self.label) };
        let speech = self.decoded.bytes as f64 / f64::from(rate * 2);
        let first_pcm = if self.decoded_apart { format!(", first PCM {} ms", ms(self.decoded.first)) } else { String::new() };
        tracing::info!(
            "TTS '{}'{label}: first byte {} ms{first_pcm}, {} bytes in {} ms, {speech:.2} s of speech",
            shown(text),
            ms(self.received.first),
            self.received.bytes,
            self.started.elapsed().as_millis()
        );
    }
}

/// A chunk's text for a log line: its start
fn shown(text: &str) -> String {
    let start: String = text.chars().take(LOGGED_TEXT_CHARS).collect();
    if text.chars().count() > LOGGED_TEXT_CHARS { format!("{start}…") } else { start }
}

async fn next_pcm(started: &mut VecDeque<Pcm>) -> Option<Result<Bytes>> {
    match started.front_mut() {
        Some(pcm) => pcm.recv().await,
        None => std::future::pending().await,
    }
}

/// Writes whole 16-bit samples: a sample split between two pieces of a
/// chunk's PCM waits for its second byte
async fn write_samples(input: &mut ChildStdin, odd_byte: &mut Option<u8>, pcm: &[u8]) -> std::io::Result<()> {
    let mut samples = Vec::with_capacity(pcm.len() + 1);
    samples.extend(odd_byte.take());
    samples.extend_from_slice(pcm);
    if samples.len() % 2 == 1 {
        *odd_byte = samples.pop();
    }
    input.write_all(&samples).await
}
