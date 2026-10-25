mod agent;
mod audio;
mod config;
mod openrouter;
mod prompt;
mod proto;
mod uniproxy;

use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::routing::get;
use axum::Router;
use prompt::PromptGenerator;

use crate::prompt::builtin_directive_functions;
use crate::uniproxy::UniproxyContext;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env();
    let openrouter = openrouter::OpenRouter::new(
        &config.openrouter_base_url,
        &config.openrouter_api_key,
        config.openrouter_timeout_ms,
    )?;
    let context = Arc::new(UniproxyContext {
        prompt_generator: PromptGenerator::new(
            &config.processor_prompt_path,
            &config.processor_state_prompt_path,
        ),
        functions: builtin_directive_functions(),
        openrouter,
        config,
    });

    let port = context.config.port;
    let app = Router::new()
        .route(
            "/uniproxy.alice.yandex.net/uni.ws",
            get(|State(context): State<Arc<UniproxyContext>>, upgrade: WebSocketUpgrade| async move {
                upgrade.on_upgrade(move |socket| uniproxy::handle_socket(socket, context))
            }),
        )
        .route("/health", get(|| async { "ok" }))
        .with_state(context);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("uni.ws listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
