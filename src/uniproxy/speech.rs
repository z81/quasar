//! The speech of one answer part: its text chunks are synthesized in order,
//! the next ones while the current one is still being encoded, and their PCM
//! goes through one ffmpeg encoder whose OGG/Opus pages make the part's single
//! LazyTtsStreaming stream (one TtsSpeak, one closing StreamControl).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::UniproxyContext;
use super::outbox::Outbox;
use crate::audio::Transcoder;

/// Chunks synthesized ahead of the one being encoded, so the next words are
/// ready by the time the current ones end
const PREFETCH: usize = 2;

/// Upper bound for one part's speech: a stalled synthesis must not keep the
/// stream and ffmpeg alive forever
const SPEECH_LIFETIME: Duration = Duration::from_secs(120);

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
    let (encoder, input, mut output) = Transcoder::pcm_to_ogg(context.config.tts_pcm_sample_rate)?;
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

/// Requests the synthesis of a chunk; its PCM comes through the returned
/// channel as it arrives
fn synthesize(context: &Arc<UniproxyContext>, text: String, syntheses: &mut JoinSet<()>) -> Pcm {
    let (pcm, receiver) = mpsc::unbounded_channel();
    let context = context.clone();
    syntheses.spawn(async move {
        let config = &context.config;
        tracing::debug!("synthesizing '{text}'");
        let speech = context
            .openrouter
            .speech_stream(&config.tts_model, &config.tts_voice, &config.tts_instructions, config.tts_speed, &text)
            .await;
        match speech {
            Ok(stream) => {
                tokio::pin!(stream);
                while let Some(piece) = stream.next().await {
                    let failed = piece.is_err();
                    if pcm.send(piece).is_err() || failed {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = pcm.send(Err(error.context("speech synthesis request failed")));
            }
        }
    });
    receiver
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
