use std::collections::HashSet;

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
    #[serde(default)]
    pub alarms: Vec<Alarm>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger: Trigger,
    pub timezone: String,
    pub time: String,
    pub weekdays: Vec<u8>,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Manual,
    Time,
    Connected,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub action: Action,
    /// Absent: continue in list order. Empty string: finish this branch.
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub otherwise: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Speech {
        text: String,
    },
    Command {
        text: String,
    },
    Volume {
        percent: u8,
    },
    Delay {
        seconds: u64,
    },
    Animation {
        animation: String,
        duration_ms: u64,
    },
    Light {
        target: String,
        animation: String,
        color: String,
        brightness: u8,
        duration_ms: u64,
    },
    Scenario {
        scenario_id: String,
    },
    If {
        condition: Condition,
    },
    Stop,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Connected { expected: bool },
    Dnd { expected: bool },
    Weekday { days: Vec<u8> },
    Time { start: String, end: String },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alarm {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub time: String,
    pub timezone: String,
    pub weekdays: Vec<u8>,
    #[serde(default)]
    pub date: Option<String>,
    /// Empty uses the bundled local alarm WAV. Otherwise a local absolute WAV path.
    #[serde(default)]
    pub sound: String,
    pub volume: u8,
    pub ramp_seconds: u64,
    pub snooze_minutes: u64,
    pub timeout_minutes: u64,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn time(value: &str) -> bool {
    value.len() == 5 && NaiveTime::parse_from_str(value, "%H:%M").is_ok()
}

fn days(value: &[u8]) -> bool {
    value.len() <= 7
        && value.iter().all(|d| (1..=7).contains(d))
        && value.iter().collect::<HashSet<_>>().len() == value.len()
}

impl Document {
    pub fn validate(&self) -> Result<(), String> {
        if self.scenarios.len() > 32 || self.alarms.len() > 32 {
            return Err("Не более 32 сценариев и 32 локальных будильников".into());
        }
        let ids: HashSet<_> = self.scenarios.iter().map(|s| s.id.as_str()).collect();
        if ids.len() != self.scenarios.len() {
            return Err("Повторяется идентификатор сценария".into());
        }
        for scenario in &self.scenarios {
            if !identifier(&scenario.id)
                || scenario.name.trim().is_empty()
                || scenario.name.len() > 200
                || scenario.timezone.parse::<chrono_tz::Tz>().is_err()
                || !time(&scenario.time)
                || !days(&scenario.weekdays)
                || (scenario.trigger == Trigger::Time && scenario.weekdays.is_empty())
                || scenario.nodes.len() > 64
            {
                return Err(format!(
                    "Некорректные настройки сценария «{}»",
                    scenario.name
                ));
            }
            let nodes: HashSet<_> = scenario.nodes.iter().map(|n| n.id.as_str()).collect();
            if nodes.len() != scenario.nodes.len() {
                return Err("Повторяется идентификатор шага".into());
            }
            for node in &scenario.nodes {
                if !identifier(&node.id)
                    || [&node.next, &node.otherwise].iter().any(|next| {
                        next.as_ref()
                            .is_some_and(|id| !id.is_empty() && !nodes.contains(id.as_str()))
                    })
                {
                    return Err("Переход ссылается на отсутствующий шаг".into());
                }
                let valid = match &node.action {
                    Action::Speech { text } | Action::Command { text } => {
                        !text.trim().is_empty() && text.len() <= 4000
                    }
                    Action::Volume { percent } => *percent <= 100,
                    Action::Delay { seconds } => *seconds <= 300,
                    Action::Animation {
                        animation,
                        duration_ms,
                    } => {
                        !animation.is_empty()
                            && animation.len() <= 200
                            && (100..=60_000).contains(duration_ms)
                    }
                    Action::Light {
                        target,
                        animation,
                        color,
                        brightness,
                        duration_ms,
                    } => {
                        let target = match target.as_str() {
                            "ring" => Some(crate::device_access::ring::Target::Ring),
                            "logo" => Some(crate::device_access::ring::Target::Logo),
                            _ => None,
                        };
                        target.is_some_and(|target| {
                            crate::device_access::ring::script(
                                target,
                                animation,
                                color,
                                *brightness,
                                *duration_ms,
                            )
                            .is_ok()
                        })
                    }
                    Action::Scenario { scenario_id } => ids.contains(scenario_id.as_str()),
                    Action::If { condition } => match condition {
                        Condition::Weekday { days: value } => days(value) && !value.is_empty(),
                        Condition::Time { start, end } => time(start) && time(end),
                        _ => true,
                    },
                    Action::Stop => true,
                };
                if !valid {
                    return Err(format!("Некорректный шаг {}", node.id));
                }
            }
            let edges: Vec<Vec<usize>> = scenario
                .nodes
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    if matches!(node.action, Action::Stop) {
                        return vec![];
                    }
                    let destination = |target: &Option<String>| match target {
                        None => (index + 1 < scenario.nodes.len()).then_some(index + 1),
                        Some(id) => scenario.nodes.iter().position(|node| &node.id == id),
                    };
                    let mut next: Vec<_> = destination(&node.next).into_iter().collect();
                    if matches!(node.action, Action::If { .. }) {
                        next.extend(destination(&node.otherwise));
                    }
                    next
                })
                .collect();
            if !acyclic(&edges) {
                return Err(format!(
                    "В сценарии «{}» обнаружен цикл переходов",
                    scenario.name
                ));
            }
        }
        let calls: Vec<Vec<usize>> = self
            .scenarios
            .iter()
            .map(|scenario| {
                scenario
                    .nodes
                    .iter()
                    .filter_map(|node| match &node.action {
                        Action::Scenario { scenario_id } => {
                            self.scenarios.iter().position(|s| &s.id == scenario_id)
                        }
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        if !acyclic(&calls) {
            return Err("Обнаружен цикл вызовов сценариев".into());
        }
        let alarm_ids: HashSet<_> = self.alarms.iter().map(|a| &a.id).collect();
        if alarm_ids.len() != self.alarms.len() {
            return Err("Повторяется идентификатор будильника".into());
        }
        for alarm in &self.alarms {
            let valid_date = alarm.date.as_ref().is_some_and(|d| {
                d.len() == 10 && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()
            });
            if !identifier(&alarm.id)
                || alarm.name.trim().is_empty()
                || alarm.name.len() > 200
                || !time(&alarm.time)
                || alarm.timezone.parse::<chrono_tz::Tz>().is_err()
                || !days(&alarm.weekdays)
                || (alarm.weekdays.is_empty() && !valid_date)
                || (alarm.date.is_some() && !valid_date)
                || alarm.volume > 100
                || alarm.ramp_seconds > 300
                || !(1..=60).contains(&alarm.snooze_minutes)
                || !(1..=30).contains(&alarm.timeout_minutes)
                || (!alarm.sound.is_empty()
                    && (!alarm.sound.starts_with('/')
                        || alarm.sound.len() > 300
                        || alarm.sound.contains(['\n', '\r', '|', '\0'])
                        || !alarm.sound.to_lowercase().ends_with(".wav")))
            {
                return Err(format!(
                    "Некорректные настройки будильника «{}»",
                    alarm.name
                ));
            }
        }
        Ok(())
    }
}

fn acyclic(edges: &[Vec<usize>]) -> bool {
    fn visit(node: usize, edges: &[Vec<usize>], colors: &mut [u8]) -> bool {
        match colors[node] {
            1 => return false,
            2 => return true,
            _ => {}
        }
        colors[node] = 1;
        for next in &edges[node] {
            if !visit(*next, edges, colors) {
                return false;
            }
        }
        colors[node] = 2;
        true
    }
    let mut colors = vec![0; edges.len()];
    (0..edges.len()).all(|index| visit(index, edges, &mut colors))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub scenario_id: String,
    pub name: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub steps: Vec<String>,
}
