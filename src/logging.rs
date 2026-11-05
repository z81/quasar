//! Reloadable log filtering; the environment is only the initial baseline.

use std::sync::OnceLock;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{reload, EnvFilter, Registry};

static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

pub fn init() {
    let initial = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(initial);
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
    let _ = FILTER.set(handle);
}

/// Config parsing already validates the filter. Keep the active logger intact
/// if its subscriber has gone away during shutdown.
pub fn apply(value: &str) {
    let Some(handle) = FILTER.get() else {
        return;
    };
    let Ok(filter) = EnvFilter::try_new(value) else {
        return;
    };
    if handle.reload(filter).is_err() {
        tracing::warn!("Cannot apply the configured log filter");
    }
}
