use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::{DndError, Schedule};

pub(super) struct Window {
    pub active: bool,
    pub next_transition: Option<DateTime<Utc>>,
    pub next_enabled: Option<bool>,
}

pub(super) fn validate(mut schedule: Schedule) -> Result<Schedule, DndError> {
    let start = time(&schedule.start)?;
    let end = time(&schedule.end)?;
    if start == end {
        return Err(DndError::Invalid(
            "Начало и конец тихого времени должны различаться".into(),
        ));
    }
    if schedule.weekdays.is_empty() || schedule.weekdays.iter().any(|day| !(1..=7).contains(day)) {
        return Err(DndError::Invalid(
            "Выберите дни начала: от 1 (понедельник) до 7 (воскресенье)".into(),
        ));
    }
    schedule.weekdays.sort_unstable();
    if schedule.weekdays.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(DndError::Invalid("Дни недели не должны повторяться".into()));
    }
    Ok(schedule)
}

fn time(value: &str) -> Result<NaiveTime, DndError> {
    let parsed = NaiveTime::parse_from_str(value, "%H:%M")
        .map_err(|_| DndError::Invalid("Укажите время в формате ЧЧ:ММ".into()))?;
    if value.len() != 5 || parsed.format("%H:%M").to_string() != value {
        return Err(DndError::Invalid("Укажите время в формате ЧЧ:ММ".into()));
    }
    Ok(parsed)
}

pub(super) fn window(
    schedule: &Schedule,
    zone: Tz,
    now: DateTime<Utc>,
) -> Result<Window, DndError> {
    if !schedule.enabled {
        return Ok(Window {
            active: false,
            next_transition: None,
            next_enabled: None,
        });
    }
    let start = time(&schedule.start)?;
    let end = time(&schedule.end)?;
    let today = now.with_timezone(&zone).date_naive();
    let mut intervals = Vec::new();
    for offset in -1..=8 {
        let date = today
            .checked_add_signed(Duration::days(offset))
            .ok_or(DndError::Unavailable)?;
        if !schedule
            .weekdays
            .contains(&(date.weekday().number_from_monday() as u8))
        {
            continue;
        }
        let end_date = if end < start {
            date.succ_opt().ok_or(DndError::Unavailable)?
        } else {
            date
        };
        if let (Some(start), Some(end)) = (
            resolve(zone, date.and_time(start), false),
            resolve(zone, end_date.and_time(end), true),
        ) {
            if start < end {
                intervals.push((start, end));
            }
        }
    }
    let active_at = |at| {
        intervals
            .iter()
            .any(|(start, end)| *start <= at && at < *end)
    };
    let mut transitions: Vec<_> = intervals
        .iter()
        .flat_map(|(start, end)| [*start, *end])
        .collect();
    transitions.sort_unstable();
    let next_transition = transitions
        .into_iter()
        .find(|at| *at > now && active_at(*at) != active_at(*at - Duration::milliseconds(1)));
    Ok(Window {
        active: active_at(now),
        next_enabled: next_transition.map(active_at),
        next_transition,
    })
}

/// Cover a repeated clock hour once, without a temporary exit from quiet time.
/// A missing DST boundary advances to the first real minute, including the end
/// boundary, so a clock jump cannot leave DND permanently enabled.
fn resolve(zone: Tz, local: NaiveDateTime, ending: bool) -> Option<DateTime<Utc>> {
    for minute in 0..=180 {
        let at = local.checked_add_signed(Duration::minutes(minute))?;
        match zone.from_local_datetime(&at) {
            LocalResult::Single(at) => return Some(at.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, second) => {
                return Some(
                    if ending {
                        first.max(second)
                    } else {
                        first.min(second)
                    }
                    .with_timezone(&Utc),
                );
            }
            LocalResult::None => {}
        }
    }
    None
}
