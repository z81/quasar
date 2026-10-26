//! ffmpeg-based audio transcoding: PCM -> OGG/Opus (streaming, for TTS) and
//! OGG/Opus -> WAV (for STT providers that reject Opus).

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const FFMPEG_TIMEOUT_MS: u64 = 30_000;

fn spawn_ffmpeg(arguments: &[String]) -> Result<Child> {
    Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn ffmpeg (is it installed?)")
}

fn pcm_to_ogg_arguments(sample_rate: u32) -> Vec<String> {
    [
        "-f", "s16le", "-ar", &sample_rate.to_string(), "-ac", "1", "-i", "pipe:0",
        "-c:a", "libopus", "-b:a", "32k", "-application", "voip",
        // one 20 ms Opus packet per OGG page: by default the muxer holds the
        // audio pages of a short phrase until its PCM ends, so nothing could
        // play while it is still being synthesized
        "-page_duration", "20000",
        "-flush_packets", "1", "-f", "ogg", "pipe:1",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// Decodes an OGG/Opus utterance into a 16 kHz mono 16-bit WAV file — the
/// input every speech recognition provider accepts
pub async fn ogg_to_wav(ogg: &[u8]) -> Result<Vec<u8>> {
    let arguments = ["-i", "pipe:0", "-f", "s16le", "-ar", "16000", "-ac", "1", "pipe:1"]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let pcm = run_buffered(&arguments, ogg).await?;

    let mut wav = wav_header(pcm.len(), 16_000);
    wav.extend_from_slice(&pcm);
    Ok(wav)
}

fn wav_header(data_length: usize, sample_rate: u32) -> Vec<u8> {
    let byte_rate = sample_rate * 2;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&((36 + data_length) as u32).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // mono
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes()); // 16-bit
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(data_length as u32).to_le_bytes());
    header
}

async fn run_buffered(arguments: &[String], input: &[u8]) -> Result<Vec<u8>> {
let mut child = spawn_ffmpeg(arguments)?;
    let mut stdin = child.stdin.take().context("ffmpeg stdin")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let mut output = Vec::new();

    let write = async {
        stdin.write_all(input).await?;
        // ChildStdin::shutdown is a no-op; dropping the handle closes the pipe
        drop(stdin);
        Ok::<(), anyhow::Error>(())
    };
    let read = async {
        let mut buffer = vec![0; 16_384];
        loop {
            let read = stdout.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..read]);
        }
        Ok::<(), anyhow::Error>(())
    };

let result = tokio::time::timeout(Duration::from_millis(FFMPEG_TIMEOUT_MS), async {
        let (write_result, read_result) = tokio::join!(write, read);
        let status = child.wait().await.context("waiting for ffmpeg")?;
        if !status.success() {
            bail!("ffmpeg failed with status {status}");
        }
        write_result?;
        read_result?;
        Ok(())
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => {
            let _ = child.kill().await;
            bail!("ffmpeg timed out after {FFMPEG_TIMEOUT_MS}ms")
        }
    }?;

    Ok(output)
}

/// A PCM -> OGG/Opus encoder driven by its owner: signed 16-bit little-endian
/// mono PCM goes into `input`, OGG pages come out of `output` as soon as they
/// are encoded. Dropping the encoder kills ffmpeg
pub struct OggEncoder {
    child: Child,
}

impl OggEncoder {
    pub fn spawn(sample_rate: u32) -> Result<(Self, ChildStdin, ChildStdout)> {
        let mut child = spawn_ffmpeg(&pcm_to_ogg_arguments(sample_rate))?;
        let input = child.stdin.take().context("ffmpeg stdin")?;
        let output = child.stdout.take().context("ffmpeg stdout")?;
        Ok((Self { child }, input, output))
    }

    /// Waits for ffmpeg to exit once its input is closed and its output drained
    pub async fn finish(mut self) -> Result<()> {
        let status = self.child.wait().await.context("waiting for ffmpeg")?;
        if !status.success() {
            bail!("ffmpeg transcode failed with status {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn decodes_ogg_to_valid_wav() {
        let ogg = generate_silence_ogg().await;
        let wav = ogg_to_wav(&ogg).await.unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert!(wav.len() > 44);
    }

    /// ffmpeg generates a short silent OGG/Opus buffer
    async fn generate_silence_ogg() -> Vec<u8> {
        let arguments = [
            "-f", "lavfi", "-i", "anullsrc=r=16000:cl=mono", "-t", "0.3",
            "-c:a", "libopus", "-f", "ogg", "pipe:1",
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
        run_buffered(&arguments, &[]).await.unwrap()
    }
}
