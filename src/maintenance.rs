//! Snapshots of panel-managed station files. Only archives created here can be
//! restored; arbitrary uploaded archives and the station's rootkeeper are never extracted.
use crate::{api::ApiState, auth::AuthManager, device_access::operations as ops};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Component, Path as FilePath},
    sync::Arc,
};

const DIRECTORY: &str = "/data/quasar/quasar-panel-backups";
const MAX_BYTES: usize = 32 * 1024 * 1024;
const ROOTS: [&str; 3] = [
    "data/quasar/quasar-panel",
    "data/quasar/alice-automations",
    "data/yam",
];

#[derive(Clone)]
struct Context {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    id: String,
    created_at: String,
    bytes: usize,
    sha256: String,
    roots: Vec<String>,
}
#[derive(Deserialize)]
struct Operation {
    action: String,
    id: Option<String>,
    target: Option<ops::Target>,
    #[serde(default)]
    confirmed: bool,
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/devices/{duid}/maintenance", get(list).post(change))
        .route("/devices/{duid}/maintenance/{id}", get(download))
        .with_state(Context { api, auth })
}

fn key(duid: &str) -> String {
    format!("maintenance:snapshots:{duid}")
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
}
async fn saved(context: &Context, duid: &str) -> Result<Vec<Snapshot>, String> {
    let value = context
        .api
        .storage
        .get_control_value(&key(duid))
        .await
        .map_err(|_| "Не удалось прочитать список копий")?;
    value
        .map(serde_json::from_value)
        .transpose()
        .map(|v| v.unwrap_or_default())
        .map_err(|_| "Список копий повреждён".into())
}
async fn save(context: &Context, duid: &str, values: &[Snapshot]) -> Result<(), String> {
    context
        .api
        .storage
        .set_control_value(&key(duid), &json!(values))
        .await
        .map_err(|_| "Не удалось сохранить список копий".into())
}
async fn run(target: &ops::Target, script: &str) -> Result<String, String> {
    let (code, bytes) = ops::execute(target, script).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if code == 0 {
        Ok(text)
    } else {
        Err(format!(
            "Операция остановлена (код {code}): {}",
            text.chars().take(1800).collect::<String>()
        ))
    }
}
async fn list(
    State(context): State<Context>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = ops::authorize(&context.api, &context.auth, &duid, &headers, false).await
    {
        return response;
    }
    match saved(&context,&duid).await {
        Ok(snapshots)=>Json(json!({"snapshots":snapshots,"persistent":context.api.storage.is_persistent(),"max_bytes":MAX_BYTES})).into_response(),
        Err(error)=>ops::error(StatusCode::SERVICE_UNAVAILABLE,error),
    }
}

async fn snapshot(context: &Context, duid: &str, target: &ops::Target) -> Result<Snapshot, String> {
    let mut values = saved(context, duid).await?;
    if values.len() >= 20 {
        return Err("Уже сохранено 20 копий. Скачайте и удалите ненужные.".into());
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let archive = format!("{DIRECTORY}/{id}.tar.gz");
    let script = format!(
        r#"set -e
umask 077
[ ! -L /data/quasar ] && [ ! -L {DIRECTORY} ] || {{ echo 'Каталог копий не должен быть ссылкой'; exit 2; }}
mkdir -p {DIRECTORY}
free=$(df -k /data | tail -n 1 | awk '{{print $4}}')
case "$free" in ''|*[!0-9]*) echo 'Не удалось проверить свободное место'; exit 6;; esac
[ "$free" -ge 40960 ] || {{ echo 'Для копии нужно не менее 40 МиБ свободного места'; exit 6; }}
set --
for path in data/quasar/quasar-panel data/quasar/alice-automations; do
 if [ -d "/$path" ]; then [ ! -L "/$path" ] || exit 3; set -- "$@" "$path"; fi
done
if [ "$(cat /data/yam/.quasar-panel-owner 2>/dev/null)" = {owner} ]; then
 [ ! -L /data/yam ] || exit 3
 set -- "$@" data/yam
fi
[ "$#" -gt 0 ] || {{ echo 'Управляемые компоненты ещё не установлены'; exit 4; }}
trap 'rm -f {archive}.tmp' EXIT
trap 'exit 130' HUP INT TERM
# Limit both the station's temporary archive and the server-side download.
(ulimit -f 65536; cd /; tar -czf {archive}.tmp "$@")
size=$(wc -c < {archive}.tmp)
[ "$size" -le {MAX_BYTES} ] || {{ echo 'Копия превышает лимит 32 МиБ'; exit 5; }}
mv {archive}.tmp {archive}
printf '%s\n' "$@"
"#,
        owner = ops::quote(&format!("quasar-packages-v1:{duid}"))
    );
    let output = run(target, &script).await?;
    let roots: Vec<String> = output
        .lines()
        .filter(|line| ROOTS.contains(line))
        .map(str::to_owned)
        .collect();
    if roots.is_empty() {
        return Err("Не удалось определить состав копии".into());
    }
    let bytes = ops::pull(target, &archive, MAX_BYTES).await?;
    validate_archive(&bytes, &roots)?;
    let snapshot = Snapshot {
        id,
        created_at: chrono::Utc::now().to_rfc3339(),
        bytes: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        roots,
    };
    values.insert(0, snapshot.clone());
    save(context, duid, &values).await?;
    Ok(snapshot)
}

fn safe_path(path: &FilePath, roots: &[String]) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|p| matches!(p, Component::Normal(_) | Component::CurDir))
        && roots
            .iter()
            .any(|root| path == FilePath::new(root) || path.starts_with(root))
}
fn validate_archive(bytes: &[u8], roots: &[String]) -> Result<(), String> {
    const MAX_EXPANDED: u64 = 128 * 1024 * 1024;
    const MAX_FILE: u64 = 64 * 1024 * 1024;
    if bytes.len() > MAX_BYTES
        || roots.is_empty()
        || roots.iter().any(|root| !ROOTS.contains(&root.as_str()))
    {
        return Err("Неверный состав копии".into());
    }
    // Check raw records before tar parses long-name/PAX extensions. This limits
    // metadata allocations too, and consumes the gzip trailer (CRC/truncation)
    // instead of silently stopping when the first tar end marker is encountered.
    archive_preflight(bytes)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut expanded = 0_u64;
    let mut paths = std::collections::BTreeSet::new();
    let mut files = std::collections::BTreeSet::new();
    let mut links = std::collections::BTreeMap::new();
    for (index, entry) in archive
        .entries()
        .map_err(|_| "Не удалось прочитать архив")?
        .enumerate()
    {
        if index >= 10_000 {
            return Err("В копии больше 10 000 файлов".into());
        }
        let mut entry = entry.map_err(|_| "Повреждён архив копии")?;
        let path = entry
            .path()
            .map_err(|_| "Некорректный путь в копии")?
            .into_owned();
        if !safe_path(&path, roots) || !archive_name(&path) {
            return Err("Копия содержит путь вне управляемых компонентов".into());
        }
        let path = normalize_archive_path(&path).ok_or("Неверный путь в копии")?;
        if !paths.insert(path.clone()) {
            return Err("В копии повторяется путь файла".into());
        }
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            let link = entry
                .link_name()
                .map_err(|_| "Некорректная ссылка")?
                .ok_or("Пустая ссылка в копии")?;
            if !archive_name(&link) {
                return Err("Некорректный путь ссылки".into());
            }
            let joined = if kind.is_symlink() && link.is_absolute() {
                // YAM itself uses absolute /data/yam public links. Only this
                // same-root destination is allowed; no members may traverse it.
                link.strip_prefix("/")
                    .map_err(|_| "Некорректная ссылка")?
                    .to_path_buf()
            } else if kind.is_symlink() {
                path.parent().unwrap_or(FilePath::new("")).join(&link)
            } else {
                if link.is_absolute() {
                    return Err("Абсолютная жёсткая ссылка недопустима".into());
                }
                link.into_owned()
            };
            let joined =
                normalize_archive_path(&joined).ok_or("Ссылка выходит из управляемого каталога")?;
            let root = roots
                .iter()
                .find(|root| path.starts_with(root))
                .ok_or("Не найден корень ссылки")?;
            if !joined.starts_with(root) || entry.size() != 0 {
                return Err("Копия содержит ссылку вне своего компонента".into());
            }
            let raw = if kind.is_symlink() {
                let destination = entry
                    .link_name()
                    .map_err(|_| "Некорректная ссылка")?
                    .ok_or("Пустая ссылка")?;
                if destination.is_absolute() {
                    destination
                        .strip_prefix("/")
                        .map_err(|_| "Некорректная ссылка")?
                        .to_path_buf()
                } else {
                    path.parent().unwrap_or(FilePath::new("")).join(destination)
                }
            } else {
                joined.clone()
            };
            links.insert(path.clone(), (joined, kind.is_hard_link(), raw));
        } else if !kind.is_dir() && !kind.is_file() {
            return Err("В копии обнаружен специальный файл".into());
        } else if kind.is_file() {
            files.insert(path);
        } else if entry.size() != 0 {
            return Err("Каталог содержит недопустимые данные".into());
        }
        if entry.size() > MAX_FILE {
            return Err("Файл в копии превышает 64 МиБ".into());
        }
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_EXPANDED {
            return Err("Распакованная копия превышает 128 МиБ".into());
        }
        std::io::copy(&mut entry, &mut std::io::sink())
            .map_err(|_| "Оборвано содержимое файла копии")?;
    }
    for path in &paths {
        if path
            .ancestors()
            .skip(1)
            .any(|parent| links.contains_key(parent))
        {
            return Err("Файл архива расположен внутри ссылки".into());
        }
    }
    for (target, hard, raw) in links.values() {
        if *hard && !files.contains(target) {
            return Err("Жёсткая ссылка не указывает на обычный файл копии".into());
        }
        // `alias/..` is resolved by the filesystem after following alias, not
        // by lexical normalization. Refuse that ambiguous form explicitly.
        let mut prefix = std::path::PathBuf::new();
        for component in raw.components() {
            match component {
                Component::Normal(part) => prefix.push(part),
                Component::ParentDir => {
                    if prefix
                        .ancestors()
                        .any(|ancestor| links.contains_key(ancestor))
                    {
                        return Err("Ссылка использует переход вверх через другую ссылку".into());
                    }
                    prefix.pop();
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn archive_name(path: &FilePath) -> bool {
    path.to_str().is_some_and(|name| {
        !name.is_empty() && name.len() <= 1024 && !name.chars().any(char::is_control)
    })
}

fn normalize_archive_path(path: &FilePath) -> Option<std::path::PathBuf> {
    let mut result = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return None,
        }
    }
    Some(result)
}

fn archive_preflight(bytes: &[u8]) -> Result<(), String> {
    const MAX_TAR: u64 = 144 * 1024 * 1024;
    let decoder = flate2::bufread::GzDecoder::new(bytes);
    let mut input = decoder.take(MAX_TAR + 1);
    let mut headers = 0_u64;
    let mut metadata = 0_u64;
    loop {
        let mut block = [0_u8; 512];
        input
            .read_exact(&mut block)
            .map_err(|_| "Архив оборван до завершающих блоков")?;
        if block.iter().all(|byte| *byte == 0) {
            input
                .read_exact(&mut block)
                .map_err(|_| "Нет второго завершающего блока архива")?;
            if block.iter().any(|byte| *byte != 0) {
                return Err("Некорректное завершение архива".into());
            }
            break;
        }
        headers += 1;
        if headers > 20_000 {
            return Err("Слишком много заголовков архива".into());
        }
        let header = tar::Header::from_byte_slice(&block);
        let size = header
            .size()
            .map_err(|_| "Некорректный размер записи архива")?;
        if matches!(header.entry_type().as_byte(), b'x' | b'g' | b'L' | b'K') {
            if size > 16384 {
                return Err("Слишком большой расширенный заголовок архива".into());
            }
            metadata = metadata.saturating_add(size);
        } else if size > 64 * 1024 * 1024 {
            return Err("Файл в копии превышает 64 МиБ".into());
        }
        if metadata + headers * 512 > 16 * 1024 * 1024 {
            return Err("Слишком много метаданных архива".into());
        }
        let padded = size.div_ceil(512) * 512;
        let copied = std::io::copy(&mut input.by_ref().take(padded), &mut std::io::sink())
            .map_err(|_| "Оборван архив копии")?;
        if copied != padded {
            return Err("Оборвано содержимое записи архива".into());
        }
    }
    let mut padding = [0_u8; 8192];
    loop {
        let count = input
            .read(&mut padding)
            .map_err(|_| "Повреждена контрольная сумма gzip или архив оборван")?;
        if count == 0 {
            break;
        }
        if padding[..count].iter().any(|byte| *byte != 0) {
            return Err("После конца архива обнаружены дополнительные данные".into());
        }
    }
    if input.limit() == 0 {
        return Err("Распакованный архив превышает допустимый размер".into());
    }
    if !input.into_inner().into_inner().is_empty() {
        return Err("После gzip обнаружены дополнительные данные".into());
    }
    Ok(())
}

async fn archive(target: &ops::Target, snapshot: &Snapshot) -> Result<Vec<u8>, String> {
    if !valid_id(&snapshot.id) {
        return Err("Неверный идентификатор копии".into());
    }
    let path = format!("{DIRECTORY}/{}.tar.gz", snapshot.id);
    run(target, &format!("test -f {path} && test ! -L {path}")).await?;
    let bytes = ops::pull(target, &path, MAX_BYTES).await?;
    if bytes.len() != snapshot.bytes || format!("{:x}", Sha256::digest(&bytes)) != snapshot.sha256 {
        return Err("Контрольная сумма копии не совпала. Восстановление отменено.".into());
    }
    validate_archive(&bytes, &snapshot.roots)?;
    Ok(bytes)
}
async fn download(
    State(context): State<Context>,
    Path((duid, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = ops::authorize(&context.api, &context.auth, &duid, &headers, false).await
    {
        return response;
    }
    let target = match ops::resolve(&context.api, &duid, None).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let result = async {
        let snapshot = saved(&context, &duid)
            .await?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or("Копия не найдена")?;
        archive(&target, &snapshot).await
    }
    .await;
    match result {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/gzip".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=quasar-{id}.tar.gz"),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(error) => ops::error(StatusCode::CONFLICT, error),
    }
}

async fn stop_components(
    target: &ops::Target,
    duid: &str,
    roots: &[String],
    remove: bool,
) -> Result<(), String> {
    if roots.iter().any(|root| root == ROOTS[0]) {
        crate::device_tools::restore_bluetooth(target).await?;
        crate::system_sounds::restore_all(target).await?;
    }
    if roots.iter().any(|root| root == ROOTS[1]) {
        crate::automations::uninstall_offline(target).await?;
    }
    if roots.iter().any(|root| root == ROOTS[2]) {
        let script = if remove {
            crate::packages::cleanup_script(target, duid).await?
        } else {
            crate::packages::stop_services_script(target).await?
        };
        run(target, &script).await?;
    }
    Ok(())
}

async fn change(
    State(context): State<Context>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Operation>,
) -> Response {
    if let Err(response) = ops::authorize(&context.api, &context.auth, &duid, &headers, true).await
    {
        return response;
    }
    let target = match ops::resolve(&context.api, &duid, body.target.clone()).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let _guard = match ops::exclusive(&target).await {
        Ok(v) => v,
        Err(e) => return ops::error(StatusCode::CONFLICT, e),
    };
    let result = change_inner(&context, &duid, &target, body).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => ops::error(StatusCode::CONFLICT, error),
    }
}

async fn change_inner(
    context: &Context,
    duid: &str,
    target: &ops::Target,
    body: Operation,
) -> Result<serde_json::Value, String> {
    // Detached package/audio preparation jobs have station-side locks too.
    crate::packages::ensure_idle(target).await?;
    crate::device_tools::ensure_bluetooth_idle(target).await?;
    if body.action == "backup" {
        return Ok(
            json!({"snapshot":snapshot(context,duid,target).await?,"message":"Копия сохранена на колонке. Её можно скачать."}),
        );
    }
    if !body.confirmed {
        return Err("Подтвердите действие в пульте".into());
    }
    if body.action == "uninstall" {
        let backup = snapshot(context, duid, target).await?;
        stop_components(target, duid, &backup.roots, true).await?;
        run(
            target,
            "set -e\nrm -rf /data/quasar/quasar-panel /data/quasar/alice-automations",
        )
        .await?;
        return Ok(
            json!({"snapshot":backup,"message":"Управляемые компоненты удалены. Архивы и настройки сервера сохранены."}),
        );
    }
    let id = body.id.ok_or("Выберите копию")?;
    let mut values = saved(context, duid).await?;
    let selected = values
        .iter()
        .find(|value| value.id == id)
        .cloned()
        .ok_or("Копия не найдена")?;
    if !valid_id(&id) {
        return Err("Некорректный идентификатор".into());
    }
    if body.action == "delete" {
        run(target, &format!("rm -f {DIRECTORY}/{id}.tar.gz")).await?;
        values.retain(|value| value.id != id);
        save(context, duid, &values).await?;
        return Ok(json!({"message":"Копия удалена"}));
    }
    if body.action != "restore" {
        return Err("Неизвестная операция".into());
    }
    if selected.roots.iter().any(|root| root == ROOTS[2]) {
        crate::packages::ensure_restore_allowed(target, duid).await?;
    }
    let bytes = archive(target, &selected).await?;
    run(target,"set -e\nfree=$(df -k /data | tail -n 1 | awk '{print $4}')\ncase \"$free\" in ''|*[!0-9]*) echo 'Не удалось проверить свободное место'; exit 1;; esac\n[ \"$free\" -ge 204800 ] || { echo 'Для резервной копии и восстановления нужно не менее 200 МиБ свободного места'; exit 1; }").await?;
    let current=run(target,"for path in /data/quasar/quasar-panel /data/quasar/alice-automations /data/yam/.quasar-panel-owner; do [ ! -e \"$path\" ] || { printf yes; break; }; done; true").await?;
    let backup = if current.trim() == "yes" {
        Some(snapshot(context, duid, target).await?)
    } else {
        None
    };
    let staging = format!("/data/.quasar-restore-{}", uuid::Uuid::new_v4().simple());
    let root_args = selected
        .roots
        .iter()
        .map(|root| ops::quote(root))
        .collect::<Vec<_>>()
        .join(" ");
    run(
        target,
        &format!("set -e\numask 077\nmkdir {staging}\nmkdir {staging}/old {staging}/new"),
    )
    .await?;
    ops::push(target, &format!("{staging}/archive.tar.gz"), &bytes).await?;
    run(target,&format!("set -e\ntar -xzf {staging}/archive.tar.gz -C {staging}/new\nfor path in {root_args}; do test -d {staging}/new/\"$path\"; test ! -L {staging}/new/\"$path\"; done")).await?;
    if selected.roots.iter().any(|root| root == ROOTS[1]) {
        run(target, &format!("set -e\nd={staging}/new/data/quasar/alice-automations\nrm -f \"$d/active\" \"$d/snooze\" \"$d/snooze.new\"\nrm -rf \"$d/commands\"\nmkdir -m 700 \"$d/commands\"")).await?;
    }
    stop_components(target, duid, &selected.roots, false).await?;
    // Move complete directories on /data. An interrupted swap rolls back any
    // already-moved root, while the verified pre-operation archive is retained.
    run(
        target,
        &format!(
            r#"set -e
stage={staging}
rollback() {{
 for path in {root_args}; do
  name=$(printf '%s' "$path" | tr '/' '_')
  if [ -f "$stage/moved-$name" ]; then rm -rf "/$path"; fi
  if [ -d "$stage/old/$path" ]; then mv "$stage/old/$path" "/$path"; fi
 done
}}
trap 'rollback' EXIT
trap 'exit 130' HUP INT TERM
for path in {root_args}; do
 name=$(printf '%s' "$path" | tr '/' '_')
 mkdir -p "$stage/old/$(dirname "$path")" "/$(dirname "$path")"
 if [ -e "/$path" ]; then mv "/$path" "$stage/old/$path"; fi
 touch "$stage/moved-$name"
 mv "$stage/new/$path" "/$path"
done
trap - EXIT HUP INT TERM
rm -rf "$stage"
"#
        ),
    )
    .await?;
    Ok(
        json!({"snapshot":backup,"message":"Файлы восстановлены. Компоненты остановлены: включите нужные службы, звуки и будильники в их разделах. Автозапуск настраивается заново."}),
    )
}
