use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let input: Vec<u8> = std::fs::read("/tmp/silence.ogg")?;
    let mut child = Command::new("ffmpeg")
        .arg("-hide_banner").arg("-loglevel").arg("error")
        .args(["-i", "pipe:0", "-f", "s16le", "-ar", "16000", "-ac", "1", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    eprintln!("spawned");
    let write = async {
        stdin.write_all(&input).await?;
        stdin.shutdown().await?;
        eprintln!("write done");
        Ok::<(), anyhow::Error>(())
    };
    let read = async {
        let mut buffer = vec![0u8; 16384];
        let mut total = 0;
        loop {
            let n = stdout.read(&mut buffer).await?;
            if n == 0 { break }
            total += n;
        }
        eprintln!("read done: {total} bytes");
        Ok::<(), anyhow::Error>(())
    };
    let (w, r) = tokio::join!(write, read);
    eprintln!("joined");
    w?; r?;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
    eprintln!("status: {status:?}");
    Ok(())
}
