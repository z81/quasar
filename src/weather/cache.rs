//! Bounded, per-key request coalescing with stale data and outage backoff.

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use chrono::{SecondsFormat, Utc};
use tokio::{sync::Mutex, time::Instant};

const CAPACITY: usize = 128;
const FRESH_FOR: Duration = Duration::from_secs(15 * 60);
const STALE_FOR: Duration = Duration::from_secs(6 * 60 * 60);
const RETRY_AFTER: Duration = Duration::from_secs(60);

pub(super) struct Cached<T> {
    pub value: Option<T>,
    pub fetched_at: Option<String>,
    pub stale: bool,
    pub error: Option<String>,
}

impl<T> Cached<T> {
    fn unavailable(error: String) -> Self {
        Self {
            value: None,
            fetched_at: None,
            stale: false,
            error: Some(error),
        }
    }

    pub fn fresh(self) -> anyhow::Result<T> {
        if let Some(error) = self.error {
            anyhow::bail!(error);
        }
        self.value
            .ok_or_else(|| anyhow::anyhow!("Сервис погоды временно недоступен"))
    }
}

struct Entry<T> {
    value: Option<T>,
    fetched: Option<(Instant, String)>,
    retry_at: Option<Instant>,
    error: Option<String>,
}

impl<T: Clone> Entry<T> {
    fn snapshot(&self) -> Cached<T> {
        match (&self.value, &self.fetched) {
            (Some(value), Some((at, fetched_at))) if at.elapsed() <= STALE_FOR => Cached {
                value: Some(value.clone()),
                fetched_at: Some(fetched_at.clone()),
                stale: at.elapsed() >= FRESH_FOR,
                error: self.error.clone(),
            },
            _ => Cached::unavailable(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Сервис погоды временно недоступен".to_owned()),
            ),
        }
    }
}

struct Slot<T> {
    used_at: Instant,
    entry: Arc<Mutex<Entry<T>>>,
}

pub(super) struct Cache<T> {
    entries: Mutex<HashMap<String, Slot<T>>>,
}

impl<T: Clone> Cache<T> {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_or_fetch<F, Fut>(&self, key: String, fetch: F) -> Cached<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        let entry = {
            let mut entries = self.entries.lock().await;
            if !entries.contains_key(&key) && entries.len() >= CAPACITY {
                // Never evict an entry while a caller is fetching or waiting for it.
                let oldest = entries
                    .iter()
                    .filter(|(_, slot)| Arc::strong_count(&slot.entry) == 1)
                    .min_by_key(|(_, slot)| slot.used_at)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                } else {
                    return Cached::unavailable(
                        "Сервис погоды занят. Повторите запрос чуть позже".to_owned(),
                    );
                }
            }
            let slot = entries.entry(key).or_insert_with(|| Slot {
                used_at: Instant::now(),
                entry: Arc::new(Mutex::new(Entry {
                    value: None,
                    fetched: None,
                    retry_at: None,
                    error: None,
                })),
            });
            slot.used_at = Instant::now();
            slot.entry.clone()
        };
        // Holding only this key's lock coalesces concurrent misses. Cancellation
        // releases it normally, without leaving a stuck "fetch in progress" flag.
        let mut entry = entry.lock().await;
        let fresh = entry
            .fetched
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < FRESH_FOR);
        let backing_off = entry.retry_at.is_some_and(|at| Instant::now() < at);
        if fresh || backing_off {
            return entry.snapshot();
        }
        match fetch().await {
            Ok(value) => {
                entry.value = Some(value);
                entry.fetched = Some((
                    Instant::now(),
                    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                ));
                entry.retry_at = None;
                entry.error = None;
            }
            Err(error) => {
                entry.retry_at = Some(Instant::now() + RETRY_AFTER);
                entry.error = Some(error.to_string());
            }
        }
        entry.snapshot()
    }
}
