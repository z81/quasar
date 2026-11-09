//! Explicit owner-managed overlays for existing firmware sounds. Uploads never select a shell path.
mod audio;
mod shell;

use crate::{
    api::ApiState,
    auth::AuthManager,
    device_access::operations::{self, Target},
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

const MAX_UPLOAD: usize = 8 * 1024 * 1024;
const MAX_SOUND: usize = 16 * 1024 * 1024;
const DIRECTORY: &str = "/data/quasar/quasar-panel/sounds";
const ORIGINALS: &str = "/data/quasar/quasar-panel/backups/sounds-original";

struct SoundState {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
    changes: Mutex<()>,
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/devices/{duid}/system-sounds", get(list).post(apply))
        .route(
            "/devices/{duid}/system-sounds/{group}/{name}",
            get(preview).post(replace).delete(restore),
        )
        .layer(DefaultBodyLimit::max(MAX_UPLOAD))
        .with_state(Arc::new(SoundState {
            api,
            auth,
            changes: Mutex::new(()),
        }))
}

#[derive(Deserialize, Default)]
struct Options {
    host: Option<String>,
    port: Option<u16>,
    #[serde(default)]
    original: bool,
    #[serde(default)]
    persistent: bool,
}

impl Options {
    fn target(&self) -> Option<Target> {
        self.host.as_ref().map(|host| Target {
            host: host.clone(),
            port: self.port.unwrap_or(5555),
        })
    }
}

fn validate(group: &str, name: &str) -> Result<(), Response> {
    if !matches!(group, "common" | "ru-RU")
        || name.is_empty()
        || name.len() > 180
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        || ![".wav", ".mp3", ".opus", ".ogg"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
    {
        return Err(operations::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Выберите существующий системный звук из списка",
        ));
    }
    Ok(())
}

async fn target(
    state: &SoundState,
    duid: &str,
    headers: &HeaderMap,
    options: &Options,
    mutation: bool,
) -> Result<Target, Response> {
    operations::authorize(&state.api, &state.auth, duid, headers, mutation).await?;
    operations::resolve(&state.api, duid, options.target()).await
}

async fn list(
    State(state): State<Arc<SoundState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Query(options): Query<Options>,
) -> Response {
    let target = match target(&state, &duid, &headers, &options, false).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _guard = state.changes.lock().await;
    match operations::execute(&target, shell::LIST).await {
        Ok((0, bytes)) => {
            let mut entries = Vec::new();
            for line in String::from_utf8_lossy(&bytes).lines() {
                let values: Vec<_> = line.split('|').collect();
                if values.len() != 5 || validate(values[0], values[1]).is_err() {
                    continue;
                }
                entries.push(json!({"group":values[0],"name":values[1],"bytes":values[2].parse::<u64>().unwrap_or(0),"custom":values[3]=="1","persistent":values[4]=="1"}));
            }
            Json(json!({"items":entries,"message":"Предпрослушивание звучит только в браузере. Изменение применяется к следующим обращениям прошивки к файлу."})).into_response()
        }
        Ok((_, bytes)) => operations::error(
            StatusCode::CONFLICT,
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(1000)
                .collect::<String>(),
        ),
        Err(error) => operations::error(StatusCode::BAD_GATEWAY, error),
    }
}

async fn preview(
    State(state): State<Arc<SoundState>>,
    Path((duid, group, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(options): Query<Options>,
) -> Response {
    if let Err(r) = validate(&group, &name) {
        return r;
    }
    let target = match target(&state, &duid, &headers, &options, false).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _guard = state.changes.lock().await;
    let stock = format!("/system/vendor/quasar/sounds/{group}/{name}");
    let original = format!("{ORIGINALS}/{group}/{name}");
    let path = if options.original {
        match operations::execute(
            &target,
            &format!("test -f {}", operations::quote(&original)),
        )
        .await
        {
            Ok((0, _)) => original,
            _ => stock,
        }
    } else {
        stock
    };
    let bytes = match operations::pull(&target, &path, MAX_SOUND).await {
        Ok(b) => b,
        Err(e) => return operations::error(StatusCode::BAD_GATEWAY, e),
    };
    let mime = match audio::format(&bytes) {
        Ok("wav") => "audio/wav",
        Ok("mp3") => "audio/mpeg",
        Ok("ogg") => "audio/ogg",
        _ => {
            return operations::error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Формат звука не поддерживается браузером",
            )
        }
    };
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn replace(
    State(state): State<Arc<SoundState>>,
    Path((duid, group, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(options): Query<Options>,
    body: Bytes,
) -> Response {
    if let Err(r) = validate(&group, &name) {
        return r;
    }
    let target = match target(&state, &duid, &headers, &options, true).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _exclusive = match operations::exclusive(&target).await {
        Ok(guard) => guard,
        Err(e) => return operations::error(StatusCode::CONFLICT, e),
    };
    if body.is_empty() || body.len() > MAX_UPLOAD {
        return operations::error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Загрузите WAV, MP3 или Opus размером до 8 МиБ",
        );
    }
    if let Err(e) = audio::format(&body) {
        return operations::error(StatusCode::UNSUPPORTED_MEDIA_TYPE, e);
    }
    let _guard = state.changes.lock().await;
    let stock = format!("/system/vendor/quasar/sounds/{group}/{name}");
    let original = format!("{ORIGINALS}/{group}/{name}");
    let source = match operations::execute(
        &target,
        &format!("test -f {}", operations::quote(&original)),
    )
    .await
    {
        Ok((0, _)) => &original,
        _ => &stock,
    };
    let original_bytes = match operations::pull(&target, source, MAX_SOUND).await {
        Ok(b) => b,
        Err(e) => return operations::error(StatusCode::BAD_GATEWAY, e),
    };
    let (encoded, metadata) = match audio::convert(&original_bytes, &body).await {
        Ok(value) => value,
        Err(e) => return operations::error(StatusCode::UNPROCESSABLE_ENTITY, e),
    };
    let token = uuid::Uuid::new_v4().simple().to_string();
    let incoming = format!("{DIRECTORY}/incoming-{token}");
    if let Err(e) = checked(
        &target,
        &format!("umask 077; mkdir -p {}", operations::quote(DIRECTORY)),
    )
    .await
    {
        return operations::error(StatusCode::BAD_GATEWAY, e);
    }
    if let Err(e) = operations::push(&target, &incoming, &encoded).await {
        return operations::error(StatusCode::BAD_GATEWAY, e);
    }
    let script = shell::replace(&group, &name, &incoming, &token, options.persistent);
    let result = checked(&target, &script).await;
    // Only our randomly named staging file is removed, even after a failed transaction.
    let _ = operations::execute(&target, &format!("rm -f {}", operations::quote(&incoming))).await;
    match result {
        Ok(message) => {
            Json(json!({"message":message,"audio":metadata,"persistent":options.persistent}))
                .into_response()
        }
        Err(error) => operations::error(StatusCode::CONFLICT, error),
    }
}

async fn restore(
    State(state): State<Arc<SoundState>>,
    Path((duid, group, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(options): Query<Options>,
) -> Response {
    if let Err(r) = validate(&group, &name) {
        return r;
    }
    let target = match target(&state, &duid, &headers, &options, true).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _exclusive = match operations::exclusive(&target).await {
        Ok(guard) => guard,
        Err(e) => return operations::error(StatusCode::CONFLICT, e),
    };
    let _guard = state.changes.lock().await;
    match checked(&target, &shell::restore(&group, &name)).await {
        Ok(message) => Json(json!({"message":message})).into_response(),
        Err(e) => operations::error(StatusCode::CONFLICT, e),
    }
}

async fn apply(
    State(state): State<Arc<SoundState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Query(options): Query<Options>,
) -> Response {
    let target = match target(&state, &duid, &headers, &options, true).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _exclusive = match operations::exclusive(&target).await {
        Ok(guard) => guard,
        Err(e) => return operations::error(StatusCode::CONFLICT, e),
    };
    let _guard = state.changes.lock().await;
    match apply_saved(&target).await{Ok(())=>Json(json!({"message":"Сохранённые подмены применены; автозапуск следует настройкам каждого файла."})).into_response(),Err(e)=>operations::error(StatusCode::CONFLICT,e)}
}

async fn checked(target: &Target, script: &str) -> Result<String, String> {
    let (code, output) = operations::execute(target, script).await?;
    let message = String::from_utf8_lossy(&output)
        .trim()
        .chars()
        .take(1500)
        .collect::<String>();
    if code == 0 {
        Ok(message)
    } else {
        Err(if message.is_empty() {
            "Операция колонки не выполнена".into()
        } else {
            message
        })
    }
}

/// The caller authorizes component removal and holds the station's exclusive guard.
pub(crate) async fn restore_all(target: &Target) -> Result<(), String> {
    checked(
        target,
        &format!("[ -d {DIRECTORY}/entries ] || exit 0\n{}", shell::cleanup()),
    )
    .await
    .map(|_| ())
}

pub(crate) async fn apply_saved(target: &Target) -> Result<(), String> {
    checked(target, &shell::apply_saved()).await.map(|_| ())
}
