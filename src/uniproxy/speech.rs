//! The speech of one answer part: its text chunks are synthesized in order,
//! the next ones while the current one is still being encoded, and their PCM
//! goes through one ffmpeg encoder whose OGG/Opus pages make the part's single
//! LazyTtsStreaming stream (one TtsSpeak, one closing StreamControl). Speech
//! that is not raw PCM at the encoder's rate — MP3 (OPENROUTER_TTS_FORMAT=mp3),
//! Piper's WAV (TTS_PROVIDER=piper) — is decoded chunk by chunk, each by its
//! own ffmpeg, to PCM at one rate whatever the provider's. A chunk Piper
//! fails on is spoken by OpenRouter instead (PIPER_FALLBACK).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use super::outbox::Outbox;
use super::UniproxyContext;
use crate::audio::{Encoded, Transcoder};
use crate::config::{TtsFormat, TtsProvider};
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

/// The fixed text a VoiceDesign voice is cloned from. Qwen's Base clones from
/// a 3-second clip; this one lasts about 4.5 s, enough for a stable speaker
/// without delaying the first answer audio much.
const DESIGN_REFERENCE_TEXT: &str =
    "Привет! Меня зовут Алиса. Я готова помочь с любым вопросом.";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum SpeechStyle {
    #[default]
    Normal,
    Whisper,
}

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
    style: SpeechStyle,
    voice: Option<&str>,
    style_instruction: Option<&str>,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let (chunks, receiver) = mpsc::unbounded_channel();
    let _ = chunks.send(text.to_string());
    drop(chunks);
    speak(context, outbox, receiver, reference_message_id, speech_end, style, voice, style_instruction).await
}

/// Speaks the chunks of a part through one TTS stream, closed once the
/// chunks end; on a failure, an abort or a timeout the stream is closed too
pub async fn speak(
    context: &Arc<UniproxyContext>,
    outbox: &Outbox,
    chunks: mpsc::UnboundedReceiver<String>,
    reference_message_id: &str,
    speech_end: Option<SpeechEnd>,
    style: SpeechStyle,
    voice: Option<&str>,
    style_instruction: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(if style == SpeechStyle::Whisper { crate::whisper::available(&context.config) } else { context.can_speak() }, "Синтез речи не настроен в настройках сервера Quasar");
    let speech = stream_speech(context, outbox, chunks, reference_message_id, speech_end, style, voice, style_instruction);
    match tokio::time::timeout(SPEECH_LIFETIME, speech).await {
        Ok(result) => result,
        Err(_) => Err(anyhow!(
            "TTS streaming took longer than {SPEECH_LIFETIME:?}"
        )),
    }
}

async fn stream_speech(
    context: &Arc<UniproxyContext>,
    outbox: &Outbox,
    mut chunks: mpsc::UnboundedReceiver<String>,
    reference_message_id: &str,
    mut speech_end: Option<SpeechEnd>,
    style: SpeechStyle,
    _voice: Option<&str>,
    style_instruction: Option<&str>,
) -> Result<()> {
    // dropping the guard closes the stream, dropping the encoder kills ffmpeg
    let stream = outbox.open_tts_stream(reference_message_id)?;
    let (encoder, input, mut output) = Transcoder::pcm_to_ogg(speech_rate(context, style))?;
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
    // A VoiceDesign style gets one cloned Base voice for the whole part, so
    // every chunk keeps the same speaker. The reference is generated while
    // the LLM is still writing the first chunks.
    let prepared = match (style, style_instruction) {
        (SpeechStyle::Normal, Some(instruction))
            if context.design_tts.is_some() && context.local_tts.is_some() =>
        {
            let context = context.clone();
            let instruction = instruction.to_owned();
            Some(tokio::spawn(async move { prepare_design_voice(&context, &instruction).await }))
        }
        _ => None,
    };
    tokio::try_join!(feed(context, &mut chunks, input, style, style_instruction, prepared), drain)?;
    encoder.finish().await
}

/// Writes the chunks' PCM into ffmpeg in their order, synthesizing up to
/// PREFETCH chunks ahead of the one being written
async fn feed(
    context: &Arc<UniproxyContext>,
    chunks: &mut mpsc::UnboundedReceiver<String>,
    mut input: ChildStdin,
    style: SpeechStyle,
    style_instruction: Option<&str>,
    mut prepared: Option<JoinHandle<Result<DesignVoice>>>,
) -> Result<()> {
    // dropping the set (on an abort) cancels the requests still running
    let mut syntheses = JoinSet::new();
    let mut waiting = VecDeque::new();
    let mut started: VecDeque<Pcm> = VecDeque::new();
    let mut design_voice = None;
    let mut more_text = true;
    let mut odd_byte = None;
    loop {
        while started.len() <= PREFETCH {
            let Some(text) = waiting.pop_front() else {
                break;
            };
            if let Some(prepared) = prepared.take() {
                match prepared.await {
                    Ok(Ok(voice)) => design_voice = Some(voice.name),
                    Ok(Err(error)) => {
                        tracing::warn!("VoiceDesign reference failed: {error:#}; the chunks keep separate voices")
                    }
                    Err(error) => {
                        tracing::warn!("VoiceDesign reference task failed: {error}; the chunks keep separate voices")
                    }
                }
            }
            started.push_back(synthesize(
                context,
                text,
                style,
                design_voice.clone(),
                style_instruction.map(|value| value.to_owned()),
                &mut syntheses,
            ));
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
                    if style == SpeechStyle::Whisper {
                        // A normal-voice fallback or silently skipped whisper
                        // would break the user's explicit quiet response mode.
                        return Err(error);
                    }
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
    match (context.config.tts_provider, context.config.tts_format) {
        (TtsProvider::OpenRouter, TtsFormat::Pcm) => context.config.tts_pcm_sample_rate,
        _ => SPEECH_RATE,
    }
}

/// Whisper is always 24 kHz: local Qwen3-TTS streams it, ElevenLabs is decoded
/// to it. Normal speech follows the provider's rate.
fn speech_rate(context: &UniproxyContext, style: SpeechStyle) -> u32 {
    if style == SpeechStyle::Whisper { SPEECH_RATE } else { pcm_rate(context) }
}

/// Requests the synthesis of a chunk; its PCM comes through the returned
/// channel as it arrives. Dropping the set aborts the task, which kills its
/// decoder
fn synthesize(
    context: &Arc<UniproxyContext>,
    text: String,
    style: SpeechStyle,
    local_voice: Option<String>,
    style_instruction: Option<String>,
    syntheses: &mut JoinSet<()>,
) -> Pcm {
    let (pcm, receiver) = mpsc::unbounded_channel();
    let context = context.clone();
    syntheses.spawn(async move {
        tracing::debug!("synthesizing '{text}'");
        let started = Instant::now();
        let synthesized = if style == SpeechStyle::Whisper {
            synthesize_whisper(&context, &text, &pcm, started).await
        } else { match context.config.tts_provider {
            TtsProvider::Piper => match &context.piper {
                Some(piper) => synthesize_piper(&context, piper, &text, &pcm, started).await,
                None => synthesize_openrouter(&context, &text, &pcm, started).await,
            },
            TtsProvider::Local => {
                if let Some(voice) = local_voice.as_deref() {
                    synthesize_local(&context, &text, &pcm, started, voice).await
                } else if let (Some(instruction), Some(_)) = (style_instruction.as_deref(), context.design_tts.as_ref()) {
                    match synthesize_design(&context, &text, &pcm, started, instruction).await {
                        Ok(synthesis) => Ok(synthesis),
                        Err(error) => {
                            tracing::warn!("design TTS failed: {error:#}");
                            synthesize_local(&context, &text, &pcm, started, &context.config.local_tts_voice).await
                        }
                    }
                } else {
                    synthesize_local(&context, &text, &pcm, started, &context.config.local_tts_voice).await
                }
            }
            TtsProvider::OpenRouter => synthesize_openrouter(&context, &text, &pcm, started).await,
        }};
        match synthesized {
            Ok(synthesis) => synthesis.log(&text, speech_rate(&context, style)),
            Err(error) => {
                let failed = format!("synthesis failed after {} ms", started.elapsed().as_millis());
                let _ = pcm.send(Err(error.context(failed)));
            }
        }
    });
    receiver
}

/// Local Qwen3-TTS streams 24 kHz PCM and needs no delivery tag. Eleven v3/v4
/// interpret the delivery tag as acoustic whispering; their MP3 is decoded to
/// 24 kHz. Whisper failures never fall back to a normal speaking voice.
async fn synthesize_whisper(
    context: &UniproxyContext,
    text: &str,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
) -> Result<Synthesis> {
    if let Some(local) = &context.whisper_tts {
        let speech = local
            .speech_stream(
                &context.config.whisper_tts_local_model,
                &context.config.whisper_tts_local_voice,
                &None,
                1.0,
                TtsFormat::Pcm,
                text,
                None,
            )
            .await?;
        return Ok(forward(speech, pcm, started).await?.labelled("whisper-local".to_owned()));
    }
    let decoder = Transcoder::decode_to_pcm(Encoded::Mp3, SPEECH_RATE)?;
    // Do not let square brackets from ordinary answer text insert competing
    // ElevenLabs delivery tags after the whisper tag.
    let text = format!("[whispers] {}", text.replace(['[', ']'], ""));
    let speech = context.openrouter.speech_stream(
        &context.config.whisper_tts_model,
        &context.config.whisper_tts_voice,
        &None,
        1.0,
        TtsFormat::Mp3,
        &text,
        None,
    ).await?;
    Ok(decode(decoder, speech, pcm, started, &AtomicBool::new(false)).await?
        .labelled("whisper".to_owned()))
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
    anyhow::ensure!(!config.openrouter_api_key.is_empty(), "Резервный синтез речи OpenRouter не настроен");
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
        .speech_stream(&config.tts_model, &config.tts_voice, &config.tts_instructions, config.tts_speed, config.tts_format, text, None)
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

/// The chunk by a local OpenAI-compatible TTS (Qwen3-TTS): raw PCM at 24 kHz
async fn synthesize_local(
    context: &UniproxyContext,
    text: &str,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
    voice: &str,
) -> Result<Synthesis> {
    let config = &context.config;
    let local = context.local_tts.as_ref().context("Локальный синтез речи не настроен")?;
    let speech = local.speech_stream(
        &config.local_tts_model,
        voice,
        &None,
        1.0,
        TtsFormat::Pcm,
        text,
        // Qwen's default 0.9 can substitute consonants in short Russian
        // replies; 0.65 kept the checked phrases intact with the same voice.
        Some(0.65),
    ).await?;
    Ok(forward(speech, pcm, started).await?.labelled(format!("qwen-local:{voice}")))
}

/// A Base voice cloned from the deterministic VoiceDesign reference of one
/// style. The name is derived from the style, so every chunk, part and answer
/// with that style uses the same speaker.
struct DesignVoice {
    name: String,
}

/// The full VoiceDesign instruction: the optional configured prefix, then the
/// LLM's description, with whitespace normalized for a stable key.
fn design_instructions(context: &UniproxyContext, instruction: &str) -> String {
    let prefix = context.config.design_tts_prefix.trim();
    let text = if prefix.is_empty() {
        instruction.trim().to_string()
    } else {
        format!("{prefix} {}", instruction.trim())
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A style's permanent voice name and reference seed: the same prompt always
/// gets the same reference audio, even after a restart of the Base server.
fn design_voice_key(instructions: &str) -> (String, i64) {
    let mut hasher = Sha256::new();
    hasher.update(b"qwen-design-v1\0");
    hasher.update(instructions.as_bytes());
    hasher.update(b"\0");
    hasher.update(DESIGN_REFERENCE_TEXT.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    let hash = u64::from_be_bytes(bytes) & i64::MAX as u64;
    (format!("design-{hash:016x}"), hash as i64)
}

/// Generates the style's reference voice once and registers it in Base; later
/// chunks are cloned from it, so one part never changes speakers.
async fn prepare_design_voice(context: &UniproxyContext, instruction: &str) -> Result<DesignVoice> {
    let design = context.design_tts.as_ref().context("VoiceDesign не настроен")?;
    let base = context.local_tts.as_ref().context("Локальный синтез речи не настроен")?;
    let instructions = design_instructions(context, instruction);
    let (name, seed) = design_voice_key(&instructions);
    if base.voices().await?.iter().any(|voice| voice == &name) {
        return Ok(DesignVoice { name });
    }
    let wav = design
        .speech_wav(
            &context.config.design_tts_model,
            &context.config.design_tts_voice,
            &Some(instructions),
            seed,
            DESIGN_REFERENCE_TEXT,
        )
        .await?;
    base.register_voice(&name, DESIGN_REFERENCE_TEXT, &wav).await?;
    tracing::info!("VoiceDesign reference '{name}' registered for '{}'", shown(instruction));
    Ok(DesignVoice { name })
}

/// A free-text style from the LLM: VoiceDesign generates the delivery from
/// the fixed voice prefix plus the requested manner.
async fn synthesize_design(
    context: &UniproxyContext,
    text: &str,
    pcm: &mpsc::UnboundedSender<Result<Bytes>>,
    started: Instant,
    instruction: &str,
) -> Result<Synthesis> {
    let design = context.design_tts.as_ref().context("VoiceDesign не настроен")?;
    let instructions = design_instructions(context, instruction);
    let speech = design
        .speech_stream(
            &context.config.design_tts_model,
            &context.config.design_tts_voice,
            &Some(instructions),
            1.0,
            TtsFormat::Pcm,
            text,
            None,
        )
        .await?;
    Ok(forward(speech, pcm, started).await?.labelled("qwen-design".to_owned()))
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
        let ms = |duration: Option<Duration>| {
            duration.map_or_else(
                || "-".to_string(),
                |duration| duration.as_millis().to_string(),
            )
        };
        let label = if self.label.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.label)
        };
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
