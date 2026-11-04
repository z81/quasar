use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc,
};
use chrono_tz::Tz;

use super::{Input, Kind, Result, ScheduleError};

pub(super) fn stamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(super) fn stamp_millis(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(super) fn parse_stamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| ScheduleError::Unavailable)
}

pub(super) fn validate(mut input: Input, now: DateTime<Utc>) -> Result<(Input, Option<String>)> {
    if input.kind == Kind::Timer {
        return invalid("Для таймера используйте управление таймерами");
    }
    input.title = input.title.trim().to_owned();
    if input.title.is_empty() {
        input.title = match input.kind {
            Kind::Alarm => "Будильник",
            Kind::Reminder => "Напоминание",
            Kind::Timer => "Таймер",
        }
        .to_owned();
    }
    if input.title.chars().count() > 120 || input.title.chars().any(char::is_control) {
        return invalid("Название должно содержать не больше 120 символов без переносов строк");
    }
    input.text = input.text.trim().to_owned();
    if input.kind == Kind::Reminder && (input.text.is_empty() || input.text.chars().count() > 1000)
    {
        return invalid("Текст напоминания: от 1 до 1000 символов");
    }
    if input
        .text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return invalid("Текст содержит недопустимые управляющие символы");
    }
    if input.kind == Kind::Alarm {
        input.text.clear();
    }
    if input.time.len() != 5 {
        return invalid("Укажите время в формате ЧЧ:ММ");
    }
    let time = NaiveTime::parse_from_str(&input.time, "%H:%M")
        .map_err(|_| ScheduleError::Invalid("Некорректное время; ожидается ЧЧ:ММ".into()))?;
    if time.format("%H:%M").to_string() != input.time {
        return invalid("Укажите время в формате ЧЧ:ММ");
    }
    input.timezone = input.timezone.trim().to_owned();
    let zone = input.timezone.parse::<Tz>().map_err(|_| {
        ScheduleError::Invalid("Укажите часовой пояс IANA, например Europe/Moscow".into())
    })?;
    if input.weekdays.iter().any(|day| !(1..=7).contains(day)) {
        return invalid("Дни недели должны быть числами от 1 (понедельник) до 7 (воскресенье)");
    }
    input.weekdays.sort_unstable();
    if input.weekdays.windows(2).any(|days| days[0] == days[1]) {
        return invalid("Дни недели не должны повторяться");
    }
    match (&input.date, input.weekdays.is_empty()) {
        (Some(date), true) => {
            let date = parse_date(date)?;
            let at = resolve(zone, date, time).ok_or_else(|| {
                ScheduleError::Invalid(
                    "В выбранном часовом поясе этого времени нет из-за перевода часов".into(),
                )
            })?;
            if input.enabled && at <= now {
                return invalid("Выберите дату и время в будущем");
            }
        }
        (None, false) => {}
        _ => return invalid("Укажите либо дату одного запуска, либо дни недели для повтора"),
    }
    let next = if input.enabled {
        next(&input, now)?
    } else {
        None
    };
    Ok((input, next))
}

pub(super) fn next(input: &Input, after: DateTime<Utc>) -> Result<Option<String>> {
    let zone = input
        .timezone
        .parse::<Tz>()
        .map_err(|_| ScheduleError::Unavailable)?;
    let time =
        NaiveTime::parse_from_str(&input.time, "%H:%M").map_err(|_| ScheduleError::Unavailable)?;
    if let Some(date) = &input.date {
        return Ok(resolve(zone, parse_date(date)?, time)
            .filter(|at| *at > after)
            .map(stamp));
    }
    let today = after.with_timezone(&zone).date_naive();
    // Includes the next same weekday after a DST gap; no minute-by-minute scans.
    for offset in 0..=14 {
        let date = today
            .checked_add_signed(Duration::days(offset))
            .ok_or(ScheduleError::Unavailable)?;
        if input
            .weekdays
            .contains(&(date.weekday().number_from_monday() as u8))
        {
            if let Some(at) = resolve(zone, date, time).filter(|at| *at > after) {
                return Ok(Some(stamp(at)));
            }
        }
    }
    Err(ScheduleError::Invalid(
        "Не удалось найти следующий запуск расписания".into(),
    ))
}

fn parse_date(value: &str) -> Result<NaiveDate> {
    if value.len() != 10 {
        return invalid("Укажите дату в формате ГГГГ-ММ-ДД");
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| ScheduleError::Invalid("Некорректная дата".into()))?;
    if date.format("%Y-%m-%d").to_string() != value || !(1970..=9999).contains(&date.year()) {
        return invalid("Укажите дату в формате ГГГГ-ММ-ДД");
    }
    Ok(date)
}

fn resolve(zone: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    match zone.from_local_datetime(&date.and_time(time)) {
        LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, second) => Some(first.min(second).with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(ScheduleError::Invalid(message.to_owned()))
}
