use super::{ops, run, PANEL, REGISTRY, ROOT, RUN};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const RELEASE: &str = "yam-v0.2.5";

async fn download(url: &str, maximum: usize) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .user_agent("alice-server-rs packages")
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "Не удалось связаться с официальным репозиторием YAM".to_owned())?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > maximum as u64)
    {
        return Err(format!(
            "Репозиторий YAM вернул {} или слишком большой файл",
            response.status()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Загрузка YAM прервалась".to_owned())?
    {
        if bytes.len() + chunk.len() > maximum {
            return Err("Файл YAM превышает допустимый размер".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn packages(platform: Option<&str>) -> Result<Vec<Value>, String> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().await;
    if cache
        .as_ref()
        .is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(600))
    {
        let bytes = download(&format!("{REGISTRY}/index.json"), 2 * 1024 * 1024).await?;
        let data: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Непонятный формат каталога YAM".to_owned())?;
        *cache = Some((Instant::now(), data));
    }
    let data = &cache.as_ref().unwrap().1;
    Ok(data["packages"].as_array().into_iter().flatten().filter_map(|entry| {
        let name = entry["name"].as_str()?;
        if !super::package_name(name) || data["revoked"].get(name).is_some() { return None; }
        let targets: Vec<_> = entry["targets"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let fits = platform.is_some_and(|platform| targets.is_empty() || targets.contains(&"any") || targets.contains(&platform));
        let homepage = entry["homepage"].as_str().filter(|url| reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.username().is_empty() && url.password().is_none()));
        Some(json!({"name":name,"summary":entry["summary"],"latest":entry["latest"],"targets":targets,"homepage":homepage,"fits":fits}))
    }).collect())
}

pub(super) async fn bootstrap(
    target: &ops::Target,
    duid: &str,
    platform: &str,
    arch: &str,
) -> Result<String, String> {
    run(target, &format!("test ! -e {ROOT} || {{ echo 'Каталог /data/yam уже существует; чужая установка не изменена'; exit 1; }}")).await?;
    let triple = match arch {
        "aarch64" => "aarch64-unknown-linux-musl",
        "armv7l" => "armv7-unknown-linux-musleabihf",
        _ => return Err("Архитектура не поддерживается официальными сборками YAM".into()),
    };
    let asset = format!("yam-{triple}");
    let api = format!("https://api.github.com/repos/nyakokitsu/yam/releases/tags/{RELEASE}");
    let release: Value = serde_json::from_slice(&download(&api, 256 * 1024).await?)
        .map_err(|_| "Не удалось прочитать описание релиза YAM")?;
    let base = format!("https://github.com/nyakokitsu/yam/releases/download/{RELEASE}");
    let sums = String::from_utf8(download(&format!("{base}/SHA256SUMS"), 32768).await?)
        .map_err(|_| "Неверный файл контрольных сумм")?;
    let expected = sums
        .lines()
        .find_map(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            (parts.len() == 2
                && parts[1].trim_start_matches('*') == asset
                && parts[0].len() == 64
                && parts[0].bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| parts[0].to_lowercase())
        })
        .ok_or("В релизе нет контрольной суммы нужной сборки")?;
    let digest = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["name"] == asset)
        .and_then(|entry| entry["digest"].as_str());
    if digest != Some(format!("sha256:{expected}").as_str()) {
        return Err("SHA256SUMS и контрольная сумма GitHub не совпали".into());
    }
    let binary = download(&format!("{base}/{asset}"), 16 * 1024 * 1024).await?;
    if !binary.starts_with(b"\x7fELF") || hex::encode(Sha256::digest(&binary)) != expected {
        return Err("Контрольная сумма бинарника YAM не совпала".into());
    }
    let profile = download(
        &format!(
            "https://raw.githubusercontent.com/nyakokitsu/yam/{RELEASE}/platforms/{platform}.yaml"
        ),
        8192,
    )
    .await?;
    let parsed: Value =
        serde_yaml::from_slice(&profile).map_err(|_| "Неверный профиль платформы YAM")?;
    if parsed["id"] != platform || parsed["triple"] != triple || parsed["paths"]["root"] != ROOT {
        return Err("Профиль YAM не соответствует архитектуре или расположению установки".into());
    }
    let stage = format!("{PANEL}/staging/{}", uuid::Uuid::new_v4().simple());
    run(target, &format!("umask 077; mkdir -p {stage}")).await?;
    ops::push(target, &format!("{stage}/yam"), &binary).await?;
    ops::push(target, &format!("{stage}/profile.yaml"), &profile).await?;
    let wrapper = format!("#!/bin/sh\nexport YAM_PLATFORM='{}'\nexport YAM_PROFILES='{ROOT}/platforms'\nexport YAM_ROOT='{ROOT}'\nexec '{ROOT}/bin/yam' \"$@\"\n", platform);
    ops::push(target, &format!("{stage}/yam-run"), wrapper.as_bytes()).await?;
    Ok(format!(
        r#"test ! -e {ROOT} || {{ echo 'Установка YAM уже появилась; остановлено'; exit 1; }}
actual=$(sha256sum {stage}/yam | cut -d ' ' -f 1)
test "$actual" = {expected} || {{ echo 'Контрольная сумма на колонке не совпала'; exit 1; }}
mkdir -p {ROOT}/bin {ROOT}/platforms
cp {stage}/yam {ROOT}/bin/yam
cp {stage}/yam-run {RUN}
cp {stage}/profile.yaml {ROOT}/platforms/{platform}.yaml
chmod 755 {ROOT}/bin/yam {RUN}
printf '%s\n' {owner} > {ROOT}/.quasar-panel-owner
{RUN} list
rm -rf {stage}
printf 'Установлен YAM {RELEASE}; подключите официальный реестр для установки пакетов.\n'
"#,
        owner = ops::quote(&format!("quasar-packages-v1:{duid}"))
    ))
}
