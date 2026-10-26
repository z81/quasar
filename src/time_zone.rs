//! The time zone the assistant tells the time in. Every station request names
//! the station's zone (`Application.Timezone`, an IANA name such as
//! Europe/Moscow): the zone of the request being answered, else the
//! station's last known one, else the `TZ` environment variable, else the
//! server's local zone (UTC in a container without `TZ`). Names resolve
//! through chrono-tz's embedded database, whatever tzdata the host has.

use std::collections::HashSet;
use std::fmt::Display;
use std::sync::{Mutex, OnceLock, PoisonError};

use chrono::{DateTime, Datelike, Local, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// A zone to tell the time in
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zone {
    /// the station's zone, or `TZ`
    Named(Tz),
    /// the server's local zone
    Local,
}

impl Zone {
    /// The station's zone if it names a known one, else `TZ`, else the server's local zone
    pub fn of_station(name: Option<&str>) -> Self {
        name.and_then(|name| parse(name, "the station's time zone"))
            .or_else(environment)
            .map_or(Self::Local, Self::Named)
    }

    /// The current date and time there: `DD-MM-YYYY HH:MM:SS, weekday (zone, UTC±HH:MM)`,
    /// e.g. `26-10-2026 20:05:12, понедельник (Europe/Moscow, UTC+03:00)`
    pub fn now(&self) -> String {
        match self {
            Self::Named(zone) => describe(&Utc::now().with_timezone(zone), zone.name()),
            Self::Local => describe(&Local::now(), "the server's local zone"),
        }
    }
}

/// `TZ`, read once; a leading colon is POSIX for "a zone file name"
fn environment() -> Option<Tz> {
    static ZONE: OnceLock<Option<Tz>> = OnceLock::new();
    *ZONE.get_or_init(|| {
        let name = std::env::var("TZ").ok()?;
        parse(name.strip_prefix(':').unwrap_or(&name), "TZ")
    })
}

/// An IANA name; an unknown or empty one is warned about once per distinct value
fn parse(name: &str, what: &str) -> Option<Tz> {
    if let Ok(zone) = name.trim().parse::<Tz>() {
        return Some(zone);
    }
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = WARNED.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_string());
    if first {
        tracing::warn!("unknown time zone '{name}' ({what}): the time is told in the next zone in line");
    }
    None
}

fn describe<Z: TimeZone>(now: &DateTime<Z>, zone: &str) -> String
where
    Z::Offset: Display,
{
    format!("{}, {} ({zone}, UTC{})", now.format("%d-%m-%Y %H:%M:%S"), weekday(now.weekday()), now.format("%:z"))
}

fn weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "понедельник",
        Weekday::Tue => "вторник",
        Weekday::Wed => "среда",
        Weekday::Thu => "четверг",
        Weekday::Fri => "пятница",
        Weekday::Sat => "суббота",
        Weekday::Sun => "воскресенье",
    }
}
