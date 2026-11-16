//! A bounded, process-local copy of the events already accepted by RUST_LOG.
use std::collections::VecDeque;
use std::fmt::{self, Write};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::Serialize;
use tracing::{field::Visit, Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

const CAPACITY: usize = 1000;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MESSAGE_BYTES: usize = 4096;
static RECENT: OnceLock<Mutex<Buffer>> = OnceLock::new();

#[derive(Clone, Serialize)]
pub(crate) struct Entry {
    id: u64,
    at: String,
    level: String,
    target: String,
    message: String,
    truncated: bool,
}

struct Buffer {
    started_at: String,
    entries: VecDeque<Entry>,
    bytes: usize,
    next_id: u64,
}

fn buffer() -> &'static Mutex<Buffer> {
    RECENT.get_or_init(|| Mutex::new(Buffer {
        started_at: chrono::Utc::now().to_rfc3339(),
        entries: VecDeque::new(), bytes: 0, next_id: 1,
    }))
}

#[derive(Default)]
struct Message {
    text: String,
    truncated: bool,
}
impl Write for Message {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let mut end = value.len().min(MESSAGE_BYTES.saturating_sub(self.text.len()));
        while !value.is_char_boundary(end) { end -= 1; }
        self.text.push_str(&value[..end]);
        self.truncated |= end < value.len();
        Ok(())
    }
}
impl Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if !self.text.is_empty() { let _ = self.write_char(' '); }
        if field.name() != "message" { let _ = write!(self, "{}=", field.name()); }
        let _ = write!(self, "{value:?}");
    }
}

pub(super) struct Recent;
impl<S: Subscriber> Layer<S> for Recent {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut message = Message::default();
        event.record(&mut message);
        let metadata = event.metadata();
        let mut buffer = buffer().lock().unwrap_or_else(PoisonError::into_inner);
        let entry = Entry {
            id: buffer.next_id, at: chrono::Utc::now().to_rfc3339(),
            level: metadata.level().to_string(), target: metadata.target().to_owned(),
            message: message.text, truncated: message.truncated,
        };
        buffer.next_id += 1;
        buffer.bytes += entry.message.len() + entry.target.len();
        buffer.entries.push_back(entry);
        while buffer.entries.len() > CAPACITY || buffer.bytes > MAX_BYTES {
            if let Some(old) = buffer.entries.pop_front() {
                buffer.bytes -= old.message.len() + old.target.len();
            }
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Snapshot {
    started_at: String,
    capacity: usize,
    retained: usize,
    matched: usize,
    entries: Vec<Entry>,
}

pub(crate) fn snapshot(level: &str, search: &str, limit: usize) -> Snapshot {
    let buffer = buffer().lock().unwrap_or_else(PoisonError::into_inner);
    let search = search.to_lowercase();
    let matches = |entry: &&Entry| {
        (level.is_empty() || entry.level == level)
            && (search.is_empty() || entry.message.to_lowercase().contains(&search)
                || entry.target.to_lowercase().contains(&search))
    };
    let matched = buffer.entries.iter().filter(matches).count();
    let mut entries: Vec<_> = buffer.entries.iter().rev().filter(matches).take(limit).cloned().collect();
    entries.reverse();
    Snapshot { started_at: buffer.started_at.clone(), capacity: CAPACITY,
        retained: buffer.entries.len(), matched, entries }
}
