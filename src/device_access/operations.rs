//! Bounded, owner-authorized operations on a known station over ADB.

use std::sync::OnceLock;
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use crate::{api::ApiState, auth::AuthManager};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Target {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    5555
}

pub(crate) fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error":message.into()}))).into_response()
}

pub(crate) async fn authorize(
    api: &ApiState,
    auth: &AuthManager,
    duid: &str,
    headers: &HeaderMap,
    mutation: bool,
) -> Result<(), Response> {
    if mutation {
        crate::auth::require_device_access(auth, headers).await?;
    } else {
        crate::auth::require_device_session(auth, headers).await?;
    }
    let stations = api
        .storage
        .list()
        .await
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "Хранилище недоступно"))?;
    if !stations.iter().any(|station| station.duid == duid) {
        return Err(error(StatusCode::NOT_FOUND, "Колонка не найдена"));
    }
    Ok(())
}

pub(crate) async fn resolve(
    api: &ApiState,
    duid: &str,
    target: Option<Target>,
) -> Result<Target, Response> {
    let stations = api
        .storage
        .list()
        .await
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "Хранилище недоступно"))?;
    let station = stations
        .iter()
        .find(|station| station.duid == duid)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Колонка не найдена"))?;
    let target = target.unwrap_or_else(|| Target {
        host: station.to_api_device()["ipAddress"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        port: default_port(),
    });
    if !(super::Target {
        host: target.host.clone(),
        port: target.port,
    })
    .valid()
    {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Укажите доступный адрес ADB и порт колонки",
        ));
    }
    Ok(target)
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Serialize managed changes per ADB target. Read-only polling remains available.
/// Acquire once at the HTTP operation boundary, never inside a cleanup helper.
pub(crate) async fn exclusive(target: &Target) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    use std::collections::HashMap;
    use std::sync::{Arc, Weak};
    use tokio::sync::Mutex;
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = format!("{}:{}", target.host, target.port);
    let lock = {
        let mut locks = LOCKS.get_or_init(Default::default).lock().await;
        locks.retain(|_, value| value.strong_count() > 0);
        let lock = locks
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| Arc::new(Mutex::new(())));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    };
    lock.try_lock_owned()
        .map_err(|_| "На колонке уже выполняется изменение. Дождитесь завершения.".into())
}

pub(crate) async fn execute(target: &Target, script: &str) -> Result<(i32, Vec<u8>), String> {
    static OPERATIONS: OnceLock<Semaphore> = OnceLock::new();
    if script.len() > 512 * 1024 || script.contains('\0') {
        return Err("Слишком большая команда колонки".into());
    }
    let _permit = timeout(
        Duration::from_secs(5),
        OPERATIONS.get_or_init(|| Semaphore::new(3)).acquire(),
    )
    .await
    .map_err(|_| "Колонка занята; повторите запрос".to_owned())?
    .map_err(|_| "Подключения остановлены".to_owned())?;
    let mut connection = timeout(
        Duration::from_secs(10),
        super::adb::Connection::connect(&target.host, target.port),
    )
    .await
    .map_err(|_| "Колонка не ответила по ADB за 10 секунд".to_owned())?
    .map_err(|error| format!("Не удалось подключиться по ADB: {error}"))?;
    super::shell::probe(&mut connection)
        .await
        .map_err(|_| "Оболочка колонки не поддерживает необходимые команды".to_owned())?;
    super::shell::execute(&mut connection, script)
        .await
        .map_err(|_| {
            "Операция ADB прервана или превысила время ожидания; проверьте состояние перед повтором"
                .to_owned()
        })
}

struct TransferDir(PathBuf);

impl Drop for TransferDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn transfer_dir() -> Result<TransferDir, String> {
    let path =
        std::env::temp_dir().join(format!("quasar-transfer-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir(&path)
        .await
        .map_err(|_| "Не удалось подготовить передачу файла".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Не удалось защитить временный файл".to_owned())?;
    }
    Ok(TransferDir(path))
}

fn transfer_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path.len() > 1024 || path.chars().any(char::is_control) {
        return Err("Некорректный путь файла колонки".into());
    }
    Ok(())
}

async fn adb_transfer(
    target: &Target,
    action: &str,
    first: &str,
    second: &str,
) -> Result<(), String> {
    let serial = if target.host.contains(':') {
        format!("[{}]:{}", target.host, target.port)
    } else {
        format!("{}:{}", target.host, target.port)
    };
    let result = timeout(
        Duration::from_secs(90),
        tokio::process::Command::new("adb")
            .args(["-s", &serial, action, first, second])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| "Передача файла превысила 90 секунд".to_owned())?
    .map_err(|_| "Не удалось запустить передачу ADB".to_owned())?;
    if !result.success() {
        return Err("Не удалось передать файл по ADB".into());
    }
    Ok(())
}

pub(crate) async fn push(target: &Target, path: &str, bytes: &[u8]) -> Result<(), String> {
    transfer_path(path)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("Файл больше 32 МиБ".into());
    }
    execute(target, "true").await?;
    let dir = transfer_dir().await?;
    let local = dir.0.join("payload");
    tokio::fs::write(&local, bytes)
        .await
        .map_err(|_| "Не удалось подготовить файл".to_owned())?;
    adb_transfer(
        target,
        "push",
        local.to_str().ok_or("Некорректный временный путь")?,
        path,
    )
    .await
}

pub(crate) async fn pull(target: &Target, path: &str, limit: usize) -> Result<Vec<u8>, String> {
    transfer_path(path)?;
    let limit = limit.min(32 * 1024 * 1024);
    let (status, size) =
        execute(target, &format!("test -f {0} && wc -c < {0}", quote(path))).await?;
    let size = String::from_utf8_lossy(&size).trim().parse::<usize>().ok();
    if status != 0 || !size.is_some_and(|size| size <= limit) {
        return Err("Файл недоступен или превышает допустимый размер".into());
    }
    let dir = transfer_dir().await?;
    let local = dir.0.join("payload");
    adb_transfer(
        target,
        "pull",
        path,
        local.to_str().ok_or("Некорректный временный путь")?,
    )
    .await?;
    let actual = tokio::fs::metadata(&local)
        .await
        .map_err(|_| "Не удалось прочитать файл".to_owned())?
        .len();
    if actual > limit as u64 {
        return Err("Файл изменился и превысил допустимый размер".into());
    }
    tokio::fs::read(&local)
        .await
        .map_err(|_| "Не удалось прочитать файл".to_owned())
}
