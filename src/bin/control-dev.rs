//! Serve the built UI locally and forward its management requests to ALICE_API_URL.
//! No station emulator, LLM client, database, or dotenv files are started here.

#[path = "../control_ui.rs"]
mod control_ui;

use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, put};

#[derive(Clone)]
struct Proxy {
    client: reqwest::Client,
    base: String,
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
            && url.query().is_none()
            && url.fragment().is_none(),
        "ALICE_API_URL must be an HTTP(S) base URL without a query or fragment"
    );
    let proxy = Proxy {
        base: url.as_str().trim_end_matches('/').to_string(),
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
    };
    let app = control_ui::router()
        .route("/devices", get(forward))
        .route("/devices/{duid}", patch(forward))
        .route("/devices/{duid}/{action}", get(forward).post(forward))
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
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
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
        let local = parts
            .headers
            .get(header::HOST)
            .and_then(|host| host.to_str().ok())
            .map(|host| format!("http://{host}"));
        // Preserve the browser's origin check across this loopback development
        // proxy. Cross-origin requests are forwarded unchanged and rejected.
        if local.as_deref() == origin.to_str().ok() {
            let upstream_origin = reqwest::Url::parse(&proxy.base)
                .unwrap()
                .origin()
                .ascii_serialization();
            request = request.header(header::ORIGIN, upstream_origin);
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
