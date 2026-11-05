//! Shell streams through the official ADB server's local smart-socket API.
//! Device framing, legacy handshakes and authentication remain with adb.

use std::net::Ipv6Addr;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::timeout;

const SERVER: (&str, u16) = ("127.0.0.1", 5037);

pub(crate) struct Connection {
    stream: TcpStream,
}

impl Connection {
    pub(crate) async fn connect(host: &str, port: u16) -> Result<Self> {
        let status = Command::new("adb")
            // Without -a, tcp:5037 binds only to loopback. Older adb releases
            // treat an explicit host in -L as remote and refuse to start it.
            .args(["-L", "tcp:5037", "start-server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await
            .context("не удалось запустить adb; установите Android platform-tools на сервере")?;
        ensure!(
            status.success(),
            "не удалось запустить локальный сервер ADB"
        );
        let serial = if host.parse::<Ipv6Addr>().is_ok() {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut control = TcpStream::connect(SERVER)
            .await
            .context("локальный сервер ADB недоступен")?;
        request(&mut control, &format!("host:connect:{serial}")).await?;
        let response = response_text(&mut control).await?;
        if !(response.starts_with("connected to ") || response.starts_with("already connected to "))
        {
            return Err(device_error(&response));
        }
        drop(control);

        let mut stream = TcpStream::connect(SERVER).await?;
        stream.set_nodelay(true)?;
        request(&mut stream, &format!("host:transport:{serial}")).await?;
        // Empty shell: is the interactive PTY service supported by old station
        // firmware. shell,v2/raw are absent on these devices.
        request(&mut stream, "shell:").await?;
        Ok(Self { stream })
    }

    pub(crate) async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        let mut data = [0_u8; 8192];
        let count = self.stream.read(&mut data).await?;
        Ok((count > 0).then(|| data[..count].to_vec()))
    }

    pub(crate) async fn input(&mut self, data: &[u8]) -> Result<()> {
        timeout(Duration::from_secs(5), self.stream.write_all(data))
            .await
            .context("ADB write timed out")??;
        Ok(())
    }
}

// AOSP smart socket: four ASCII hex length bytes, service, OKAY or FAIL.
// These setup reads are only cancelled when the entire socket is discarded.
async fn request(stream: &mut TcpStream, service: &str) -> Result<()> {
    ensure!(service.len() <= 0xffff, "слишком длинный запрос ADB");
    stream
        .write_all(format!("{:04x}{service}", service.len()).as_bytes())
        .await?;
    let mut status = [0_u8; 4];
    stream.read_exact(&mut status).await?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => Err(device_error(&response_text(stream).await?)),
        _ => bail!("некорректный ответ локального сервера ADB"),
    }
}

async fn response_text(stream: &mut TcpStream) -> Result<String> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await?;
    let length = usize::from_str_radix(std::str::from_utf8(&length)?, 16)?;
    ensure!(length <= 8192, "слишком длинный ответ ADB");
    let mut data = vec![0_u8; length];
    stream.read_exact(&mut data).await?;
    Ok(String::from_utf8(data)?)
}

fn device_error(message: &str) -> anyhow::Error {
    if message.contains("unauthorized") || message.contains("authenticate") {
        anyhow::anyhow!("колонка требует подтверждения ключа ADB сервера Quasar")
    } else if message.contains("refused") {
        anyhow::anyhow!(
            "порт на колонке закрыт; включите сетевой ADB и проверьте порт (обычно 5555)"
        )
    } else {
        let message: String = message
            .chars()
            .filter(|c| !c.is_control())
            .take(300)
            .collect();
        anyhow::anyhow!("{message}")
    }
}
