use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::device_access::operations::{self, Target};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Sample {
    pub at: i64,
    pub cpu_percent: Option<f64>,
    pub temperature_c: Option<f64>,
    pub memory_used_mb: Option<f64>,
    pub memory_total_mb: Option<f64>,
    pub disk_used_mb: Option<f64>,
    pub disk_total_mb: Option<f64>,
    pub wifi_dbm: Option<f64>,
    pub load: Option<f64>,
    pub uptime_seconds: Option<f64>,
}

pub(super) fn sections(text: &str) -> BTreeMap<&str, String> {
    let mut parts = BTreeMap::<&str, String>::new();
    let mut name = "";
    for line in text.lines() {
        if let Some(key) = line.trim().strip_prefix("@@") {
            name = key;
        } else {
            let part = parts.entry(name).or_default();
            part.push_str(line.trim());
            part.push('\n');
        }
    }
    parts
}

fn number(value: Option<&str>) -> Option<f64> {
    value?.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn cpu(value: &str) -> Option<(u64, u64)> {
    let values = value
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    Some((
        values.iter().sum(),
        values[3] + values.get(4).copied().unwrap_or(0),
    ))
}

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

pub(super) async fn collect(target: &Target) -> Result<(Sample, Value), String> {
    let (code, output) = operations::execute(target, include_str!("probe.sh")).await?;
    if code != 0 {
        return Err("Не удалось прочитать состояние колонки".into());
    }
    let text = String::from_utf8_lossy(&output);
    let parts = sections(&text);
    let get = |key: &str| parts.get(key).map(String::as_str).unwrap_or_default();
    let cpu_percent = cpu(get("cpu1"))
        .zip(cpu(get("cpu2")))
        .and_then(|((a, ai), (b, bi))| {
            let total = b.checked_sub(a)?;
            let idle = bi.checked_sub(ai)?;
            (total > 0).then(|| rounded(100.0 * (total.saturating_sub(idle)) as f64 / total as f64))
        });
    let memory = get("memory")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.trim_end_matches(':'), number(fields.next())?))
        })
        .collect::<BTreeMap<_, _>>();
    let total = memory.get("MemTotal").copied();
    let available = memory.get("MemAvailable").copied().or_else(|| {
        Some(
            memory.get("MemFree")?
                + memory.get("Buffers").unwrap_or(&0.0)
                + memory.get("Cached").unwrap_or(&0.0),
        )
    });
    let disk = get("disk").split_whitespace().collect::<Vec<_>>();
    let wifi = get("wireless")
        .lines()
        .find(|line| line.contains(':'))
        .and_then(|line| {
            number(
                line.split_whitespace()
                    .nth(3)
                    .map(|value| value.trim_end_matches('.')),
            )
        });
    let sample = Sample {
        at: Utc::now().timestamp(),
        cpu_percent,
        temperature_c: get("temperature")
            .lines()
            .filter_map(|line| number(Some(line)))
            .map(|value| {
                if value.abs() > 500.0 {
                    value / 1000.0
                } else {
                    value
                }
            })
            .filter(|value| (-40.0..=150.0).contains(value))
            .max_by(f64::total_cmp)
            .map(rounded),
        memory_total_mb: total.map(|value| rounded(value / 1024.0)),
        memory_used_mb: total
            .zip(available)
            .map(|(total, free)| rounded((total - free).max(0.0) / 1024.0)),
        disk_total_mb: number(disk.get(1).copied()).map(|value| rounded(value / 1024.0)),
        disk_used_mb: number(disk.get(2).copied()).map(|value| rounded(value / 1024.0)),
        wifi_dbm: wifi,
        load: number(get("load").split_whitespace().next()),
        uptime_seconds: number(get("uptime").split_whitespace().next()),
    };
    if sample.uptime_seconds.is_none() {
        return Err("Колонка не вернула данные /proc; проверьте доступ ADB".into());
    }
    let logs = get("logs")
        .lines()
        .filter(|name| valid_log_name(name))
        .take(100)
        .collect::<Vec<_>>();
    Ok((
        sample,
        json!({
            "model":get("model").trim(), "kernel":get("kernel").trim(),
            "bluetooth": !get("bluetooth").trim().is_empty(), "logs":logs,
        }),
    ))
}

pub(super) fn valid_log_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 160
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

pub(super) async fn live(target: &Target) -> Result<Value, String> {
    let (code, output) = operations::execute(target, include_str!("live.sh")).await?;
    if code != 0 {
        return Err("Не удалось прочитать дисплей и подсветку".into());
    }
    let text = String::from_utf8_lossy(&output);
    let parts = sections(&text);
    let mut segments = BTreeMap::new();
    for line in parts
        .get("clock")
        .into_iter()
        .flat_map(|value| value.lines())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            continue;
        }
        let name = match fields[0] {
            "p3" => "PLUS1".into(),
            "q3" => "PLUS2".into(),
            "deg" => "DEGREE".into(),
            other => other.to_ascii_uppercase(),
        };
        if !CLOCK_NAMES.contains(&name.as_str()) {
            continue;
        }
        if let Some((value, max)) =
            number(fields.get(1).copied()).zip(number(fields.get(2).copied()))
        {
            if max > 0.0 {
                segments.insert(name, (value / max).clamp(0.0, 1.0));
            }
        }
    }
    let pixels = |index, name: &str, count: usize| -> Option<Vec<[u8; 3]>> {
        let metadata = parts.get(format!("fb{index}").as_str())?;
        if !metadata.lines().any(|line| line == format!("name={name}"))
            || !metadata
                .lines()
                .any(|line| line == format!("virtual_size={count},1"))
            || !metadata.lines().any(|line| line == "bits_per_pixel=24")
            || !metadata
                .lines()
                .any(|line| line == "format=8/16,8/8,8/0,0/0")
            || !metadata
                .lines()
                .any(|line| line == format!("stride={}", count * 3))
        {
            return None;
        }
        let bytes = parts
            .get(format!("pixels{index}").as_str())?
            .split_whitespace()
            .map(str::parse::<u8>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        (bytes.len() == count * 3).then(|| {
            bytes
                .chunks_exact(3)
                .map(|chunk| [chunk[2], chunk[1], chunk[0]])
                .collect()
        })
    };
    let ring = pixels(0, "ring", 24);
    let logo = pixels(1, "logo", 6);
    let available = segments.len() == CLOCK_NAMES.len() || ring.is_some() || logo.is_some();
    Ok(
        json!({"at":Utc::now().timestamp_millis(), "available":available,
        "display_available":segments.len()==CLOCK_NAMES.len(), "segments":segments,
        "ring":ring, "logo":logo,
        "reason":if available { None } else { Some("Живые кадры поддерживаются для дисплея и RGB-буферов Миди (cucumber)") }}),
    )
}

const CLOCK_NAMES: [&str; 45] = [
    "A1", "B1", "C1", "D1", "E1", "F1", "G1", "A2", "B2", "C2", "D2", "E2", "F2", "G2", "A3", "B3",
    "C3", "D3", "E3", "F3", "G3", "A4", "B4", "C4", "D4", "E4", "F4", "G4", "PLUS1", "PLUS2",
    "DEGREE", "H1", "H2", "L1", "L2", "M1", "M2", "P1", "P2", "P4", "P5", "Q1", "Q2", "Q4", "Q5",
];
