//! Serve the built UI locally and forward its management requests to ALICE_API_URL.
//! No station emulator, LLM client, database, or dotenv files are started here.

#[path = "../control_ui.rs"]
mod control_ui;

use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, put};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message as UpstreamMessage,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const MAX_WS_MESSAGE: usize = 1024 * 1024;
const WS_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const WS_SESSION_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
struct Proxy {
    client: reqwest::Client,
    base: String,
    origin: String,
    local_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut port = 8080_u16;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args
                    .next()
                    .context("--port requires a port number")?
                    .parse()?;
            }
            "--help" | "-h" => {
                println!("ALICE_API_URL=http://localhost:31116 cargo run --bin control-dev -- [--port 8080]");
                return Ok(());
            }
            _ => bail!("Unknown argument; use --help"),
        }
    }
    let base = std::env::var("ALICE_API_URL")
        .context("Set ALICE_API_URL to the management API of the running Quasar server")?;
    let url = reqwest::Url::parse(&base).context("ALICE_API_URL must be an HTTP(S) URL")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "ALICE_API_URL must be an HTTP(S) base URL without a query or fragment"
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let proxy = Proxy {
        base: url.as_str().trim_end_matches('/').to_string(),
        origin: url.origin().ascii_serialization(),
        local_port: listener.local_addr()?.port(),
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
    };
    let app = control_ui::router()
        .route("/devices", get(forward))
        .route("/devices/{duid}", patch(forward))
        .route("/devices/{duid}/{action}", get(forward).post(forward).put(forward))
        .route("/devices/{duid}/terminal", get(forward_websocket))
        .route("/devices/{duid}/files", get(forward_websocket))
        .route("/devices/{duid}/weather/location", put(forward))
        .route("/devices/{duid}/schedules/{id}", any(forward))
        .route("/devices/{duid}/schedules/{id}/{action}", any(forward))
        .route("/devices/{duid}/timers/{id}", any(forward))
        .route("/devices/{duid}/timers/{id}/{action}", any(forward))
        .route("/weather/{*path}", get(forward))
        .route("/control/{*path}", any(forward))
        .route("/auth/{*path}", any(forward))
        .route("/settings/{*path}", any(forward))
        .with_state(proxy);
    println!("Quasar control: http://{}/ui/", listener.local_addr()?);
    println!("Management requests go to ALICE_API_URL; this is not a demo API.");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn forward(State(proxy): State<Proxy>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, 2 * 1024 * 1024).await {
        Ok(body) => body,
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Request body exceeds 2 MiB").into_response()
        }
    };
    let uri = parts.uri.path_and_query().map_or("/", |uri| uri.as_str());
    let mut request = proxy
        .client
        .request(parts.method, format!("{}{uri}", proxy.base));
    for name in [
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::AUTHORIZATION,
        header::COOKIE,
        header::HeaderName::from_static("sec-fetch-site"),
    ] {
        if let Some(value) = parts.headers.get(&name) {
            request = request.header(name, value);
        }
    }
    if let Some(origin) = parts.headers.get(header::ORIGIN) {
        // Preserve the browser's origin check across this loopback development
        // proxy. Cross-origin requests are forwarded unchanged and rejected.
        if local_origin(&proxy, &parts.headers) {
            request = request.header(header::ORIGIN, &proxy.origin);
        } else {
            request = request.header(header::ORIGIN, origin);
        }
    }
    let upstream =
        match request.body(body).send().await {
            Ok(response) => response,
            Err(_) => return (
                StatusCode::BAD_GATEWAY,
                "Связь с API потеряна. Доставка не подтверждена; проверьте колонку перед повтором.",
            )
                .into_response(),
        };
    let mut response = Response::builder()
        .status(upstream.status())
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(content_type) = upstream.headers().get(header::CONTENT_TYPE) {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    for cookie in upstream.headers().get_all(header::SET_COOKIE) {
        response = response.header(header::SET_COOKIE, cookie);
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap()
}

/// Only the actual loopback listener can have its browser Origin rewritten.
/// Comparing Origin to an arbitrary Host alone would allow DNS-rebinding hosts.
fn local_origin(proxy: &Proxy, headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| reqwest::Url::parse(value).ok())
    else {
        return false;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(local) = reqwest::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    origin.scheme() == "http"
        && matches!(origin.host_str(), Some("localhost" | "127.0.0.1"))
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
        && local.username().is_empty()
        && local.password().is_none()
        && local.path() == "/"
        && local.query().is_none()
        && local.fragment().is_none()
        && origin.host_str() == local.host_str()
        && origin.port_or_known_default() == Some(proxy.local_port)
        && local.port_or_known_default() == Some(proxy.local_port)
}

async fn forward_websocket(
    State(proxy): State<Proxy>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !local_origin(&proxy, &headers) {
        return (
            StatusCode::FORBIDDEN,
            "Откройте подключение из локального пульта",
        )
            .into_response();
    }
    let path = uri.path_and_query().map_or("/", |value| value.as_str());
    let Ok(mut url) = reqwest::Url::parse(&format!("{}{path}", proxy.base)) else {
        return (StatusCode::BAD_GATEWAY, "Неверный адрес API").into_response();
    };
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    if url.set_scheme(scheme).is_err() {
        return (StatusCode::BAD_GATEWAY, "Неверный адрес API").into_response();
    }
    let Ok(mut request) = url.as_str().into_client_request() else {
        return (StatusCode::BAD_GATEWAY, "Неверный адрес API").into_response();
    };
    for name in [
        header::COOKIE,
        header::AUTHORIZATION,
        header::HeaderName::from_static("sec-fetch-site"),
    ] {
        for value in headers.get_all(&name) {
            request.headers_mut().append(name.clone(), value.clone());
        }
    }
    request
        .headers_mut()
        .insert(header::ORIGIN, proxy.origin.parse().unwrap());
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_WS_MESSAGE))
        .max_frame_size(Some(MAX_WS_MESSAGE))
        .max_write_buffer_size(MAX_WS_MESSAGE * 2);
    let upstream = match timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response)))
            if response.status().is_client_error() =>
        {
            return (
                response.status(),
                "API отклонил подключение. Проверьте вход владельца.",
            )
                .into_response();
        }
        _ => {
            return (
                StatusCode::BAD_GATEWAY,
                "Не удалось подключиться к API по WebSocket",
            )
                .into_response()
        }
    };
    upgrade
        .max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE)
        .max_write_buffer_size(MAX_WS_MESSAGE * 2)
        .on_upgrade(move |socket| relay_websocket(socket, upstream))
}

async fn relay_websocket(
    mut browser: WebSocket,
    mut upstream: WebSocketStream<MaybeTlsStream<TcpStream>>,
) {
    // No detached tasks or message queues: each direction applies backpressure.
    // A close, error or timeout drops both sockets, including an active shell.
    let _ = timeout(WS_SESSION_TIMEOUT, async {
        loop {
            let next = timeout(WS_IDLE_TIMEOUT, async {
                tokio::select! {
                    message = browser.recv() => match message {
                        Some(Ok(message)) => {
                            let closing = matches!(message, Message::Close(_));
                            let message = match message {
                                Message::Text(value) => UpstreamMessage::Text(value.to_string().into()),
                                Message::Binary(value) => UpstreamMessage::Binary(value),
                                Message::Ping(value) => UpstreamMessage::Ping(value),
                                Message::Pong(value) => UpstreamMessage::Pong(value),
                                Message::Close(value) => UpstreamMessage::Close(value.map(|frame| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                    code: frame.code.into(), reason: frame.reason.to_string().into(),
                                })),
                            };
                            matches!(timeout(WS_WRITE_TIMEOUT, upstream.send(message)).await, Ok(Ok(()))) && !closing
                        }
                        _ => false,
                    },
                    message = upstream.next() => match message {
                        Some(Ok(message)) => {
                            let closing = matches!(message, UpstreamMessage::Close(_));
                            let message = match message {
                                UpstreamMessage::Text(value) => Message::Text(value.to_string().into()),
                                UpstreamMessage::Binary(value) => Message::Binary(value),
                                UpstreamMessage::Ping(value) => Message::Ping(value),
                                UpstreamMessage::Pong(value) => Message::Pong(value),
                                UpstreamMessage::Close(value) => Message::Close(value.map(|frame| CloseFrame {
                                    code: frame.code.into(), reason: frame.reason.to_string().into(),
                                })),
                                UpstreamMessage::Frame(_) => return false,
                            };
                            matches!(timeout(WS_WRITE_TIMEOUT, browser.send(message)).await, Ok(Ok(()))) && !closing
                        }
                        _ => false,
                    }
                }
            }).await;
            if !matches!(next, Ok(true)) { break; }
        }
    }).await;
    let _ = timeout(Duration::from_secs(2), browser.close()).await;
    let _ = timeout(Duration::from_secs(2), upstream.close(None)).await;
}
