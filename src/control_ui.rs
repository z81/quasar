//! Static control UI, shared by the management API and the local development proxy.

use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/ui/") }))
        .route("/ui/app.webmanifest", get(|| async { ([("content-type", "application/manifest+json")], include_str!("../web/assets/app.webmanifest")) }))
        .route("/ui/app-icon.svg", get(|| async { ([("content-type", "image/svg+xml")], include_str!("../web/assets/app-icon.svg")) }))
        .route("/ui/sw.js", get(|| async { ([("content-type", "application/javascript"), ("cache-control", "no-cache"), ("service-worker-allowed", "/ui/")], include_str!("../web/assets/service-worker.js")) }))
        .nest_service(
            "/ui",
            ServeDir::new("web/dist/public")
                .precompressed_br()
                .precompressed_gzip(),
        )
}
