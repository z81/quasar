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
use axum::routing::{get, patch, post};

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
        .route("/devices/{duid}/{action}", post(forward))
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
    for name in [header::CONTENT_TYPE, header::ACCEPT, header::AUTHORIZATION] {
        if let Some(value) = parts.headers.get(&name) {
            request = request.header(name, value);
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
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .unwrap()
}
