//! Bounded file operations in a dedicated, explicitly confirmed POSIX shell.

use std::time::Duration;

use anyhow::{bail, Context};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::{adb::Connection, ClientMessage};

pub(super) const MAX_FILE_BYTES: usize = 256 * 1024;
pub(super) const MAX_ENTRIES: usize = 500;
const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;

pub(super) enum Error {
    Request(String),
    Transport,
}

fn quote(value: &str) -> String {
    // A long pathname must not exceed the canonical TTY line buffer either.
    value
        .chars()
        .collect::<Vec<_>>()
        .chunks(256)
        .map(|chunk| {
            format!(
                "'{}'",
                chunk.iter().collect::<String>().replace('\'', "'\\''")
            )
        })
        .collect::<Vec<_>>()
        .join("\\\n")
}

fn path(value: &str) -> Result<String, Error> {
    if !value.starts_with('/') || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(Error::Request(
            "Нужен абсолютный путь до 4096 байт без управляющих символов".into(),
        ));
    }
    Ok(quote(value))
}

pub(super) async fn probe(connection: &mut Connection) -> anyhow::Result<()> {
    let (status, _) = execute(
        connection,
        "command -v base64 >/dev/null && command -v stty >/dev/null && command -v cat >/dev/null && command -v sh >/dev/null",
    )
    .await?;
    if status != 0 {
        bail!("required shell tools unavailable");
    }
    // This dedicated session switches to a non-interactive shell behind a
    // pipe. Interactive line editors otherwise redraw every byte and can lose
    // or stall large heredocs. The PTY is restored if the pipeline exits; no
    // state in a separate interactive terminal is changed.
    connection.input(b"qsr_tty=$(stty -g) && stty -echo -icanon min 1 time 0 && { cat | sh; stty \"$qsr_tty\"; }\n").await?;
    let (status, output) = execute(
        connection,
        "command -v base64 >/dev/null && printf 'quasar-shell-v1'",
    )
    .await?;
    if status != 0 || output != b"quasar-shell-v1" {
        bail!("unsupported shell");
    }
    Ok(())
}

pub(super) async fn handle(
    connection: &mut Connection,
    message: ClientMessage,
) -> Result<Value, Error> {
    match message {
        ClientMessage::RingAnimation {
            target,
            animation,
            color,
            brightness,
            duration_ms,
        } => {
            let script = super::ring::script(target, &animation, &color, brightness, duration_ms)
                .map_err(Error::Request)?;
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            let message = match status {
                0 => None,
                2 => Some("Управление выбранной подсветкой Миди на этой колонке недоступно"),
                3 => Some("Подсветка занята штатной индикацией. Повторите позже"),
                4 => Some("На верхней панели уже выполняется анимация"),
                5 => Some("Не удалось сохранить состояние подсветки"),
                6 => Some("Не удалось вывести кадр подсветки"),
                7 => Some("Не удалось полностью восстановить подсветку после показа"),
                _ => Some("Колонка не выполнила анимацию подсветки"),
            };
            if let Some(message) = message {
                return Err(Error::Request(message.into()));
            }
            let mut result = display_result(output)?;
            result["type"] = json!("ring_done");
            Ok(result)
        }
        ClientMessage::DisplayFrame {
            segments,
            brightness,
            duration_ms,
        } => {
            let script = super::display::frame_script(&segments, brightness, duration_ms)
                .map_err(Error::Request)?;
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_display_status(status)?;
            display_result(output)
        }
        ClientMessage::DisplayText {
            text,
            brightness,
            duration_ms,
        } => {
            let script =
                super::display::script(&text, brightness, duration_ms).map_err(Error::Request)?;
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_display_status(status)?;
            display_result(output)
        }
        ClientMessage::DisplayAnimation {
            animation,
            brightness,
            duration_ms,
        } => {
            let script = super::display::animation_script(&animation, brightness, duration_ms)
                .map_err(Error::Request)?;
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_display_status(status)?;
            display_result(output)
        }
        ClientMessage::List { path: value } => {
            let quoted = path(&value)?;
            let script = format!(
                "p={quoted}; [ -d \"$p\" ] || exit 2; n=0; \
                for e in \"$p\"/* \"$p\"/.[!.]* \"$p\"/..?*; do \
                [ -e \"$e\" ] || [ -L \"$e\" ] || continue; \
                if [ \"$n\" -ge {MAX_ENTRIES} ]; then printf 'T\\n'; break; fi; \
                n=$((n+1)); k=f; [ -d \"$e\" ] && k=d; \
                z=$(stat -c %s \"$e\" 2>/dev/null || stat -f %z \"$e\" 2>/dev/null) || z=0; \
                printf '%s\\t%s\\t' \"$k\" \"$z\"; \
                printf '%s' \"${{e##*/}}\" | base64 | tr -d '\\r\\n'; printf '\\n'; done"
            );
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_status(status)?;
            let mut entries = Vec::new();
            let mut truncated = false;
            for line in output.split(|byte| *byte == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line == b"T" {
                    truncated = true;
                    continue;
                }
                if line.is_empty() {
                    continue;
                }
                let line = std::str::from_utf8(line).map_err(|_| Error::Transport)?;
                let mut parts = line.splitn(3, '\t');
                let directory = parts.next() == Some("d");
                let size = parts
                    .next()
                    .and_then(|size| size.parse::<u64>().ok())
                    .unwrap_or(0);
                let name = parts
                    .next()
                    .and_then(|name| STANDARD.decode(name).ok())
                    .and_then(|name| String::from_utf8(name).ok())
                    .ok_or_else(|| {
                        Error::Request("В каталоге есть имена файлов не в UTF-8".into())
                    })?;
                entries.push(
                    json!({"path":format!("{}/{}", value.trim_end_matches('/'), name),
                    "name":name,"directory":directory,"size":size}),
                );
            }
            Ok(json!({"type":"list","path":value,"entries":entries,"truncated":truncated}))
        }
        ClientMessage::Read { path: value } => {
            let quoted = path(&value)?;
            let script = format!(
                "p={quoted}; [ -f \"$p\" ] && [ -r \"$p\" ] || exit 2; \
                z=$(wc -c < \"$p\") || exit 2; [ \"$z\" -le {MAX_FILE_BYTES} ] || exit 3; base64 < \"$p\""
            );
            let (status, output) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_status(status)?;
            let encoded: Vec<u8> = output
                .into_iter()
                .filter(|byte| !byte.is_ascii_whitespace())
                .collect();
            let decoded = STANDARD.decode(&encoded).map_err(|_| Error::Transport)?;
            if decoded.len() > MAX_FILE_BYTES {
                return Err(Error::Request("Файл больше 256 КиБ".into()));
            }
            Ok(json!({"type":"file","path":value,"data":STANDARD.encode(decoded)}))
        }
        ClientMessage::Write { path: value, data } => {
            let quoted = path(&value)?;
            let decoded = STANDARD
                .decode(&data)
                .map_err(|_| Error::Request("Некорректные данные base64".into()))?;
            if decoded.len() > MAX_FILE_BYTES {
                return Err(Error::Request("Файл больше 256 КиБ".into()));
            }
            // Use short heredoc lines so canonical TTY input limits cannot
            // truncate an uploaded file. Base64 cannot contain the delimiter.
            let encoded = STANDARD.encode(decoded);
            let encoded = encoded
                .as_bytes()
                .chunks(512)
                .map(|chunk| std::str::from_utf8(chunk).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let script = format!(
                "p={quoted}\nt=\"${{p}}.quasar-{suffix}\"\numask 077\n\
                trap 'rm -f \"$t\"' EXIT\n\
                trap 'exit 1' HUP INT TERM\n\
                [ ! -L \"$p\" ] || exit 4\n\
                if [ -e \"$p\" ]; then [ -f \"$p\" ] || exit 2; cp -p \"$p\" \"$t\" || exit 2; fi\n\
                base64 -d > \"$t\" <<'QUASAR_FILE_DATA'\n{encoded}\nQUASAR_FILE_DATA\n\
                r=$?\n[ \"$r\" -eq 0 ] || exit \"$r\"\n\
                mv -f \"$t\" \"$p\""
            );
            let (status, _) = execute(connection, &script)
                .await
                .map_err(|_| Error::Transport)?;
            ensure_status(status)?;
            Ok(json!({"type":"written","path":value}))
        }
        _ => Err(Error::Request("Неизвестная операция с файлами".into())),
    }
}

fn display_result(output: Vec<u8>) -> Result<Value, Error> {
    let outcome = std::str::from_utf8(&output)
        .map(str::trim)
        .unwrap_or_default();
    if !matches!(outcome, "completed" | "interrupted") {
        return Err(Error::Request(
            "Колонка не подтвердила результат показа".into(),
        ));
    }
    Ok(json!({"type":"display_done","outcome":outcome}))
}

fn ensure_display_status(status: i32) -> Result<(), Error> {
    let message = match status {
        0 => return Ok(()),
        2 => "Эта оболочка не поддерживает дисплей Миди или нужные команды",
        3 => "Дисплей занят системной синхронизацией. Повторите позже",
        4 => "На дисплее уже выполняется другой показ",
        5 => "Не удалось прочитать состояние сегментов дисплея",
        6 => "Не удалось записать состояние сегментов дисплея",
        7 => "Показ завершён, но восстановить дисплей полностью не удалось",
        _ => "Колонка не выполнила команду дисплея",
    };
    Err(Error::Request(message.into()))
}

fn ensure_status(status: i32) -> Result<(), Error> {
    match status {
        0 => Ok(()),
        3 => Err(Error::Request("Файл больше 256 КиБ".into())),
        4 => Err(Error::Request(
            "Для сохранения откройте сам файл, а не символьную ссылку".into(),
        )),
        _ => Err(Error::Request(
            "Не удалось выполнить операцию: проверьте путь, права и доступное место".into(),
        )),
    }
}

/// Framing uses unguessable markers emitted by printf. The echoed command only
/// contains escaped control characters and cannot be mistaken for a reply.
pub(crate) async fn execute(
    connection: &mut Connection,
    script: &str,
) -> anyhow::Result<(i32, Vec<u8>)> {
    timeout(Duration::from_secs(20), async {
        let marker = uuid::Uuid::new_v4().simple().to_string();
        let begin = format!("\x1e{marker}BEGIN\x1f");
        let end = format!("\x1e{marker}END:");
        let command = format!(
            "printf '\\036{marker}BEGIN\\037'; (\n{script}\n); qsr_status=$?; printf '\\036{marker}END:%s\\037' \"$qsr_status\"\n"
        );
        connection.input(command.as_bytes()).await?;
        let mut buffer = Vec::new();
        let mut begun = false;
        loop {
            let Some(data) = connection.read().await? else { bail!("shell closed"); };
            buffer.extend(data);
            if !begun {
                if let Some(position) = find(&buffer, begin.as_bytes()) {
                    buffer.drain(..position + begin.len());
                    begun = true;
                } else if buffer.len() > begin.len() {
                    buffer.drain(..buffer.len() - begin.len());
                }
            }
            if begun {
                if let Some(position) = find(&buffer, end.as_bytes()) {
                    let status_start = position + end.len();
                    if let Some(status_end) = buffer[status_start..].iter().position(|byte| *byte == 0x1f) {
                        let status = std::str::from_utf8(&buffer[status_start..status_start + status_end])?.parse()?;
                        buffer.truncate(position);
                        return Ok((status, buffer));
                    }
                }
            }
            if buffer.len() > MAX_COMMAND_OUTPUT { bail!("shell output limit exceeded"); }
        }
    }).await.context("shell operation timed out")?
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
