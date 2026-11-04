//! Static control UI, shared by the management API and the local development proxy.

use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/ui/") }))
        .nest_service(
            "/ui",
            ServeDir::new("web/dist/public")
                .precompressed_br()
                .precompressed_gzip(),
        )
}
