use js_sys::{Array, Date, Intl::DateTimeFormat, Object, Reflect};

fn formatter(locale: &str, timezone: &str, fields: &[(&str, &str)]) -> DateTimeFormat {
    let options = Object::new();
    let _ = Reflect::set(&options, &"timeZone".into(), &timezone.into());
    for (key, value) in fields {
        let _ = Reflect::set(&options, &(*key).into(), &(*value).into());
    }
    DateTimeFormat::new(&Array::of1(&locale.into()), &options)
}

pub fn browser_timezone() -> String {
    let formatter = DateTimeFormat::new(&Array::new(), &Object::new());
    Reflect::get(&formatter.resolved_options(), &"timeZone".into())
        .ok()
        .and_then(|zone| zone.as_string())
        .unwrap_or_else(|| "UTC".to_string())
}

pub fn tomorrow(now: &str, timezone: &str) -> String {
    let instant = Date::new(&now.into());
    let instant = if instant.get_time().is_nan() {
        Date::new_0()
    } else {
        instant
    };
    let parts = formatter(
        "en-CA",
        timezone,
        &[
            ("year", "numeric"),
            ("month", "2-digit"),
            ("day", "2-digit"),
        ],
    )
    .format_to_parts(&instant);
    let mut year = String::new();
    let mut month = String::new();
    let mut day = String::new();
    for part in parts {
        let key = Reflect::get(&part, &"type".into())
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default();
        let value = Reflect::get(&part, &"value".into())
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default();
        match key.as_str() {
            "year" => year = value,
            "month" => month = value,
            "day" => day = value,
            _ => {}
        }
    }
    // This is calendar arithmetic on an explicit UTC date, not a 24-hour addition
    // in the station's zone (which can cross daylight-saving transitions).
    let date = Date::new(&format!("{year}-{month}-{day}T12:00:00Z").into());
    date.set_utc_date(date.get_utc_date() + 1);
    format!(
        "{:04}-{:02}-{:02}",
        date.get_utc_full_year(),
        date.get_utc_month() + 1,
        date.get_utc_date()
    )
}

pub fn timestamp(value: &str, timezone: &str) -> String {
    let date = Date::new(&value.into());
    if date.get_time().is_nan() {
        return value.to_string();
    }
    formatter(
        "ru-RU",
        timezone,
        &[
            ("day", "numeric"),
            ("month", "short"),
            ("year", "numeric"),
            ("hour", "2-digit"),
            ("minute", "2-digit"),
            ("hourCycle", "h23"),
        ],
    )
    .format()
    .call1(&Object::new(), &date)
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_else(|| value.to_string())
}

pub fn calendar_date(value: &str) -> String {
    let parts = value.split('-').collect::<Vec<_>>();
    if parts.len() == 3 {
        format!("{}.{}.{}", parts[2], parts[1], parts[0])
    } else {
        value.to_string()
    }
}
