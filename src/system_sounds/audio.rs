use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn format(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        Ok("wav")
    } else if bytes.starts_with(b"OggS") {
        Ok("ogg")
    } else if bytes.starts_with(b"ID3")
        || bytes.first() == Some(&0xff) && bytes.get(1).is_some_and(|b| b & 0xe0 == 0xe0)
    {
        Ok("mp3")
    } else {
        Err("Поддерживаются RIFF/WAV, MP3 и Ogg Opus; плейлисты и ссылки не принимаются".into())
    }
}

async fn command(mut cmd: Command) -> Result<Vec<u8>, String> {
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "Обработка звука превысила 30 секунд")?
        .map_err(|_| "На сервере недоступны ffmpeg/ffprobe")?;
    if !result.status.success() {
        return Err(format!(
            "Ошибка обработки звука: {}",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(700)
                .collect::<String>()
        ));
    }
    Ok(result.stdout)
}

async fn probe(path: &Path, format: &str) -> Result<Value, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-f",
        format,
        "-select_streams",
        "a:0",
        "-show_entries",
        "stream=codec_name,sample_rate,channels:format=duration",
        "-of",
        "json",
    ])
    .arg(path);
    serde_json::from_slice(&command(cmd).await?)
        .map_err(|_| "Не удалось прочитать параметры звука".into())
}

pub(super) async fn convert(original: &[u8], input: &[u8]) -> Result<(Vec<u8>, Value), String> {
    let original_format = format(original)?;
    let input_format = format(input)?;
    let directory =
        std::env::temp_dir().join(format!("quasar-sound-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir(&directory)
        .await
        .map_err(|_| "Не удалось создать временный каталог")?;
    let work = Work(directory);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&work.0, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Не удалось защитить временные файлы")?;
    }
    let source = work.0.join("original");
    let upload = work.0.join("upload");
    let output = work.0.join("converted");
    tokio::fs::write(&source, original)
        .await
        .map_err(|_| "Не удалось подготовить оригинал")?;
    tokio::fs::write(&upload, input)
        .await
        .map_err(|_| "Не удалось подготовить загруженный звук")?;
    let original = probe(&source, original_format).await?;
    let input = probe(&upload, input_format).await?;
    let duration = input["format"]["duration"]
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 120.0)
        .ok_or("Длительность должна быть от 0 до 120 секунд")?;
    let source_codec = input["streams"][0]["codec_name"].as_str().unwrap_or("");
    if input_format == "ogg" && source_codec != "opus" {
        return Err("В контейнере Ogg поддерживается только Opus".into());
    }
    let codec = original["streams"][0]["codec_name"]
        .as_str()
        .ok_or("В оригинале отсутствует звуковая дорожка")?;
    let rate = original["streams"][0]["sample_rate"]
        .as_str()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| (8000..=192000).contains(v))
        .ok_or("Частота оригинала не поддерживается")?;
    let channels = original["streams"][0]["channels"]
        .as_u64()
        .filter(|v| (1..=8).contains(v))
        .ok_or("Число каналов оригинала не поддерживается")?;
    let (encoder, container) = match codec {
        "mp3" => ("libmp3lame", "mp3"),
        "opus" => ("libopus", "ogg"),
        "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_u8" | "pcm_f32le" => (codec, "wav"),
        _ => {
            return Err(format!(
                "Кодек оригинала {codec} пока не поддерживается; файл не изменён"
            ))
        }
    };
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-nostdin",
        "-v",
        "error",
        "-protocol_whitelist",
        "file,pipe",
        "-f",
        input_format,
        "-i",
    ])
    .arg(&upload)
    .args([
        "-map",
        "0:a:0",
        "-map_metadata",
        "-1",
        "-vn",
        "-sn",
        "-dn",
        "-t",
        "120",
        "-ar",
        &rate.to_string(),
        "-ac",
        &channels.to_string(),
        "-c:a",
        encoder,
        "-fs",
        "16777216",
        "-f",
        container,
    ])
    .arg(&output);
    command(cmd).await?;
    let bytes = tokio::fs::read(&output)
        .await
        .map_err(|_| "Не удалось прочитать преобразованный звук")?;
    if bytes.len() >= super::MAX_SOUND {
        return Err(
            "После преобразования звук превышает 16 МиБ; загрузите более короткий файл".into(),
        );
    }
    let converted = probe(&output, container).await?;
    let result_duration = converted["format"]["duration"]
        .as_str()
        .and_then(|v| v.parse::<f64>().ok())
        .ok_or("Не удалось проверить длительность результата")?;
    if (result_duration - duration).abs() > 0.25
        || converted["streams"][0]["codec_name"] != codec
        || converted["streams"][0]["sample_rate"] != rate.to_string()
        || converted["streams"][0]["channels"] != channels
    {
        return Err("Параметры результата не совпали с оригиналом; файл не изменён".into());
    }
    Ok((
        bytes,
        json!({"codec":codec,"sample_rate":rate,"channels":channels,"duration":result_duration}),
    ))
}
