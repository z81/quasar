//! User-initiated device sessions. Terminal and file access use separate ADB
//! shell connections to the rooted station.

mod adb;
mod display;
mod shell;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{timeout, Instant};

use crate::api::ApiState;
use crate::auth::AuthManager;

const MAX_SESSIONS: usize = 4;
const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SESSION_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MESSAGE_LIMIT: usize = 384 * 1024;

struct AccessState {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
    sessions: Arc<Semaphore>,
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/devices/{duid}/access-status", get(status))
        .route("/devices/{duid}/terminal", get(terminal))
        .route("/devices/{duid}/files", get(files))
        .with_state(Arc::new(AccessState {
            api,
            auth,
            sessions: Arc::new(Semaphore::new(MAX_SESSIONS)),
        }))
}

#[derive(Deserialize)]
struct Target {
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_port() -> u16 {
    5555
}

impl Target {
    fn valid(&self) -> bool {
        if self.port == 0 || self.host.is_empty() || self.host.len() > 253 {
            return false;
        }
        self.host.parse::<std::net::IpAddr>().is_ok()
            || self.host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Input {
        data: String,
    },
    Resize {},
    Shell,
    List {
        path: String,
    },
    Read {
        path: String,
    },
    Write {
        path: String,
        data: String,
    },
    DisplayText {
        text: String,
        brightness: u8,
        duration_ms: u64,
    },
    DisplayAnimation {
        animation: String,
        brightness: u8,
        duration_ms: u64,
    },
    DisplayFrame {
        segments: Vec<String>,
        brightness: u8,
        duration_ms: u64,
    },
}

impl ClientMessage {
    fn is_display(&self) -> bool {
        matches!(
            self,
            Self::DisplayText { .. } | Self::DisplayAnimation { .. } | Self::DisplayFrame { .. }
        )
    }
}

async fn authorize(
    state: &AccessState,
    duid: &str,
    headers: &HeaderMap,
    require_origin: bool,
) -> Result<(), Response> {
    if require_origin {
        crate::auth::require_device_access(&state.auth, headers).await?;
    } else {
        crate::auth::require_device_session(&state.auth, headers).await?;
    }
    match state.api.storage.list().await {
        Ok(stations) if stations.iter().any(|station| station.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(_) => Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Хранилище недоступно",
        )),
    }
}

async fn status(
    State(state): State<Arc<AccessState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &duid, &headers, false).await {
        return response;
    }
    Json(
        json!({"terminal":true,"files":true,"maxSessions":MAX_SESSIONS,
        "maxFileBytes":shell::MAX_FILE_BYTES,"maxEntries":shell::MAX_ENTRIES,
        "idleTimeoutSeconds":IDLE_TIMEOUT.as_secs()}),
    )
    .into_response()
}

async fn terminal(
    State(state): State<Arc<AccessState>>,
    Path(duid): Path<String>,
    Query(target): Query<Target>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade_session(state, duid, target, headers, upgrade, false).await
}

async fn files(
    State(state): State<Arc<AccessState>>,
    Path(duid): Path<String>,
    Query(target): Query<Target>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade_session(state, duid, target, headers, upgrade, true).await
}

async fn upgrade_session(
    state: Arc<AccessState>,
    duid: String,
    target: Target,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
    file_mode: bool,
) -> Response {
    if let Err(response) = authorize(&state, &duid, &headers, true).await {
        return response;
    }
    if !target.valid() {
        return error(
            StatusCode::BAD_REQUEST,
            "Укажите IP-адрес или имя хоста и порт от 1 до 65535",
        );
    }
    let Ok(permit) = state.sessions.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Уже открыто максимальное число подключений",
        );
    };
    // Connect after upgrade so the browser receives a useful, non-secret error.
    upgrade
        .max_message_size(MESSAGE_LIMIT)
        .max_frame_size(MESSAGE_LIMIT)
        .on_upgrade(move |socket| session(socket, state, headers, target, file_mode, permit))
}

async fn send(socket: &mut WebSocket, message: Value) -> anyhow::Result<()> {
    timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(message.to_string().into())),
    )
    .await??;
    Ok(())
}

async fn report(socket: &mut WebSocket, message: &str, recoverable: bool) -> anyhow::Result<()> {
    send(
        socket,
        json!({"type":"error","message":message,"recoverable":recoverable}),
    )
    .await
}

async fn while_connected<T>(
    socket: &mut WebSocket,
    operation: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::pin!(operation);
    loop {
        tokio::select! {
            result = &mut operation => return Some(result),
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Text(text))) if matches!(serde_json::from_str::<ClientMessage>(&text), Ok(ClientMessage::Resize { .. })) => {}
                Some(Ok(_)) => {
                    if send(socket, json!({"type":"busy","message":"Дождитесь завершения текущей операции"})).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }
}

async fn session(
    mut socket: WebSocket,
    state: Arc<AccessState>,
    headers: HeaderMap,
    target: Target,
    file_mode: bool,
    _permit: OwnedSemaphorePermit,
) {
    let Some(result) = while_connected(
        &mut socket,
        timeout(
            Duration::from_secs(10),
            adb::Connection::connect(&target.host, target.port),
        ),
    )
    .await
    else {
        return;
    };
    let mut connection = match result {
        Ok(Ok(connection)) => connection,
        result => {
            let message = match result {
                Ok(Err(error)) => {
                    tracing::warn!(%error, "ADB connection failed");
                    format!("Не удалось подключиться по ADB: {error}")
                }
                Err(_) => "Колонка не ответила на подключение ADB за 10 секунд. Проверьте адрес, порт и сеть.".to_owned(),
                Ok(Ok(_)) => unreachable!(),
            };
            let _ = report(&mut socket, &message, false).await;
            let _ = timeout(Duration::from_secs(2), socket.close()).await;
            return;
        }
    };
    if send(&mut socket, json!({"type":"connected"}))
        .await
        .is_err()
    {
        return;
    }
    let started = Instant::now();
    let mut last_input = started;
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.tick().await;
    let mut shell_ready = false;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if started.elapsed() >= SESSION_TIMEOUT || last_input.elapsed() >= IDLE_TIMEOUT {
                    let _ = report(&mut socket, "Время подключения истекло. Подключитесь снова.", false).await;
                    break;
                }
                if crate::auth::require_device_access(&state.auth, &headers).await.is_err() {
                    let _ = report(&mut socket, "Доступ к подключению завершён. Обновите страницу Quasar.", false).await;
                    break;
                }
            }
            output = connection.read(), if !shell_ready => {
                match output {
                    Ok(Some(data)) if !data.is_empty() => {
                        if send(&mut socket, json!({"type":"output","data":STANDARD.encode(data)})).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => {
                        if report(&mut socket, "Ожидалось сообщение JSON", true).await.is_err() { break; }
                        continue;
                    }
                };
                last_input = Instant::now();
                let command = match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(command) => command,
                    Err(_) => {
                        if report(&mut socket, "Неизвестная команда или неверный формат", true).await.is_err() { break; }
                        continue;
                    }
                };
                match command {
                    ClientMessage::Input { data } if !shell_ready && data.len() <= 16384 => {
                        if connection.input(data.as_bytes()).await.is_err() { break; }
                    }
                    // The station's legacy ADB shell has no resize message.
                    // Never inject stty commands into the user's active input.
                    ClientMessage::Resize {} if !shell_ready => {}
                    ClientMessage::Shell if file_mode && !shell_ready => {
                        let Some(result) = while_connected(&mut socket, shell::probe(&mut connection)).await else { break };
                        match result {
                            Ok(()) => {
                                shell_ready = true;
                                if send(&mut socket, json!({"type":"shell_ready"})).await.is_err() { break; }
                            }
                            Err(_) => {
                                let _ = report(&mut socket, "Не удалось подтвердить POSIX-оболочку с base64. Убедитесь, что открыта командная строка, и подключитесь снова.", false).await;
                                break;
                            }
                        }
                    }
                    command @ (ClientMessage::List { .. } | ClientMessage::Read { .. } | ClientMessage::Write { .. } | ClientMessage::DisplayText { .. } | ClientMessage::DisplayAnimation { .. } | ClientMessage::DisplayFrame { .. }) if shell_ready => {
                        // Revocation is also checked immediately before each file operation.
                        if crate::auth::require_device_access(&state.auth, &headers).await.is_err() { break; }
                        let display = command.is_display();
                        let Some(result) = while_connected(&mut socket, shell::handle(&mut connection, command)).await else { break };
                        match result {
                            Ok(message) => if send(&mut socket, message).await.is_err() { break; },
                            Err(shell::Error::Request(message)) => {
                                let response = if display {
                                    json!({"type":"display_error","message":message,"recoverable":true})
                                } else {
                                    json!({"type":"error","message":message,"recoverable":true})
                                };
                                if send(&mut socket, response).await.is_err() { break; }
                            }
                            Err(shell::Error::Transport) => {
                                let _ = report(&mut socket, "Соединение с оболочкой прервано или команда превысила время ожидания.", false).await;
                                break;
                            }
                        }
                    }
                    _ => if report(&mut socket, "Команда недоступна в текущем режиме подключения", true).await.is_err() { break; },
                }
            }
        }
    }
    // Dropping Connection closes the TCP socket and cancels reads. No detached
    // worker or terminal transcript survives this function.
    let _ = send(&mut socket, json!({"type":"closed"})).await;
    let _ = timeout(Duration::from_secs(2), socket.close()).await;
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}
