//! Persistent scheduling tools, executed now rather than delayed in an agent task.

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};
use chrono::{Duration, Timelike, Utc};
use serde_json::{json, Map, Value};

use crate::agent::{FunctionDefinition, FunctionKind};
use crate::connections::ConnectionRegistry;
use crate::schedules::{Input, Kind, Schedule, Scheduler};

const EDITABLE: &[&str] = &[
    "kind", "title", "text", "time", "date", "weekdays", "timezone", "enabled",
];

pub fn definitions() -> HashMap<String, FunctionDefinition> {
    let fields = json!({
        "kind":{"type":"string","enum":["alarm","reminder"]},
        "title":{"type":"string","minLength":1,"maxLength":120},
        "text":{"type":"string","description":"Required spoken text for reminders; empty for alarms"},
        "time":{"type":"string","description":"Local HH:MM; omit only when after_minutes is provided"},
        "date":{"type":["string","null"],"description":"Local YYYY-MM-DD for a one-shot event; null for repeating weekdays"},
        "weekdays":{"type":"array","items":{"type":"integer","minimum":1,"maximum":7},"uniqueItems":true,"description":"Monday=1 through Sunday=7. Empty for a one-shot date"},
        "timezone":{"type":"string","description":"Explicit IANA timezone, otherwise the station timezone"},
        "enabled":{"type":"boolean"},
        "after_minutes":{"type":"integer","minimum":1,"maximum":525600,"description":"For a one-shot reminder/alarm relative to now. Do not also pass time/date/weekdays. Rounded UP to the next minute, exact next_run_at is returned"}
    });
    let mut functions = HashMap::new();
    let mut add = |name: &str, description: &str, schema: Value| {
        functions.insert(name.to_owned(), FunctionDefinition {
            description: format!("{description} Execute immediately, NEVER use the function-call schedule field. Set can_cache=false. Confirm success only after the result; explain errors instead of claiming completion. Scheduling requires the server and station online at execution time."),
            kind: FunctionKind::Remote { has_response: true, directive: None },
            arguments_json_schema: Some(schema),
        });
    };
    add("create_schedule", "Create a persistent alarm (native ringing) or reminder (speak text) for this station. Exactly one calendar date OR repeating weekdays. For a relative request use after_minutes and let the server calculate the time. For a one-shot 'at seven' choose the next future occurrence using current date/time. Ask when the requested date/time is ambiguous.", json!({"type":"object","properties":fields,"required":["kind","title"],"additionalProperties":false}));
    let mut edit_fields = fields.as_object().unwrap().clone();
    edit_fields.insert("id".to_owned(), json!({"type":"string"}));
    add("update_schedule", "Change an existing schedule. First use list_schedules to obtain its exact id. Only supplied fields change; when changing between a date and recurrence also clear the other field. Active alarms must be stopped first.", json!({"type":"object","properties":edit_fields,"required":["id"],"additionalProperties":false}));
    add("list_schedules", "List this station's saved alarms and reminders with their exact ids, local times, next occurrence and status. Always use this before identifying an existing schedule to change/delete; never invent ids.", json!({"type":"object","properties":{},"additionalProperties":false}));
    add("delete_schedule", "Permanently remove the specified saved alarm/reminder; use its exact id from list_schedules. Do not delete when the user merely wants to stop the currently ringing alarm.", json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}));
    add("stop_alarm", "Stop this station's active alarm ringing; this stops ALL currently active managed alarms on the station but preserves future repeat schedules. Omit id to stop the current ringing. For 'stop' while an alarm is active, use this in addition to any needed player pause. Offline physical stopping waits until reconnection.", json!({"type":"object","properties":{"id":{"type":"string"}},"additionalProperties":false}));
    add("snooze_alarm", "Snooze an active alarm for ten minutes, persistently. Omit id only if exactly one alarm is active. It also stops any other alarms currently ringing on the station; only the chosen one is snoozed.", json!({"type":"object","properties":{"id":{"type":"string"}},"additionalProperties":false}));
    functions
}

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "create_schedule"
            | "update_schedule"
            | "list_schedules"
            | "delete_schedule"
            | "stop_alarm"
            | "snooze_alarm"
    )
}

pub async fn call(
    service: &Scheduler,
    registry: &ConnectionRegistry,
    duid: &str,
    name: &str,
    arguments: Option<&Value>,
) -> String {
    match execute(service, registry, duid, name, arguments).await {
        Ok(value) => value.to_string(),
        Err(error) => format!("Error: {error}. Не подтверждай действие как выполненное."),
    }
}

async fn execute(
    service: &Scheduler,
    registry: &ConnectionRegistry,
    duid: &str,
    name: &str,
    arguments: Option<&Value>,
) -> Result<Value> {
    let mut args = match arguments {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(args)) => args.clone(),
        _ => bail!("Аргументы должны быть объектом"),
    };
    match name {
        "create_schedule" => {
            let input = input(args, registry, duid)?;
            ensure!(
                input.kind != Kind::Alarm || registry.alarm_support(duid) != Some(false),
                "Колонка не поддерживает нативные будильники"
            );
            Ok(json!(service.create(duid, input).await?))
        }
        "update_schedule" => {
            let id = take_id(&mut args)?.context("Нужен id расписания из list_schedules")?;
            ensure!(!args.is_empty(), "Не указаны изменения");
            ensure!(
                args.keys()
                    .all(|key| EDITABLE.contains(&key.as_str()) || key == "after_minutes"),
                "Неизвестное поле расписания"
            );
            let current = service
                .list(duid)
                .await?
                .into_iter()
                .find(|item| item.id == id)
                .context("Расписание не найдено")?;
            let stored = serde_json::to_value(current)?;
            let mut fields: Map<String, Value> = EDITABLE
                .iter()
                .map(|key| ((*key).to_owned(), stored[*key].clone()))
                .collect();
            if args.contains_key("after_minutes") {
                for key in ["date", "time", "weekdays"] {
                    fields.remove(key);
                }
            }
            fields.extend(args);
            let input = input(fields, registry, duid)?;
            ensure!(
                input.kind != Kind::Alarm || registry.alarm_support(duid) != Some(false),
                "Колонка не поддерживает нативные будильники"
            );
            Ok(json!(service.update(duid, &id, input).await?))
        }
        "list_schedules" => {
            ensure!(args.is_empty(), "У list_schedules нет аргументов");
            Ok(
                json!({"timezone":crate::schedule_api::default_timezone(registry,duid), "items":service.list(duid).await?}),
            )
        }
        "delete_schedule" => {
            let id = take_id(&mut args)?.context("Нужен id расписания из list_schedules")?;
            ensure!(args.is_empty(), "Неизвестный аргумент");
            service.delete(duid, &id).await?;
            Ok(json!({"deleted":id}))
        }
        "stop_alarm" | "snooze_alarm" => {
            let id = take_id(&mut args)?;
            ensure!(args.is_empty(), "Неизвестный аргумент");
            let active: Vec<_> = service
                .list(duid)
                .await?
                .into_iter()
                .filter(|item| item.active_run_id.is_some())
                .collect();
            let id = match id {
                Some(id) => id,
                None if active.len() == 1 || (name == "stop_alarm" && !active.is_empty()) => {
                    active[0].id.clone()
                }
                None if active.is_empty() => bail!("Сейчас нет активного будильника"),
                None => bail!(
                    "Активно несколько будильников. Уточни, какой отложить, и используй его id"
                ),
            };
            let item = if name == "stop_alarm" {
                service.stop(duid, &id).await?
            } else {
                service.snooze(duid, &id).await?
            };
            Ok(
                json!({"schedule":item,"stops_station_alarm_channel":true,"physical_stop_confirmed":false}),
            )
        }
        _ => bail!("Неизвестная функция расписания"),
    }
}

fn input(mut args: Map<String, Value>, registry: &ConnectionRegistry, duid: &str) -> Result<Input> {
    args.entry("timezone")
        .or_insert_with(|| json!(crate::schedule_api::default_timezone(registry, duid)));
    args.entry("enabled").or_insert(json!(true));
    args.entry("text").or_insert(json!(""));
    if let Some(minutes) = args.remove("after_minutes") {
        let minutes = minutes
            .as_i64()
            .filter(|value| (1..=525600).contains(value))
            .context("after_minutes: целое число от 1 до 525600")?;
        ensure!(
            !args.contains_key("time")
                && !args.contains_key("date")
                && !args.contains_key("weekdays"),
            "Для after_minutes не передавай дату, время или дни недели"
        );
        let zone: chrono_tz::Tz = args["timezone"]
            .as_str()
            .context("Нужен часовой пояс IANA")?
            .parse()
            .context("Неизвестный часовой пояс")?;
        let mut at = Utc::now() + Duration::minutes(minutes);
        if at.second() > 0 || at.nanosecond() > 0 {
            at += Duration::seconds(i64::from(60 - at.second()));
        }
        let local = at.with_timezone(&zone);
        args.insert(
            "date".to_owned(),
            json!(local.format("%Y-%m-%d").to_string()),
        );
        args.insert("time".to_owned(), json!(local.format("%H:%M").to_string()));
    }
    args.entry("date").or_insert(Value::Null);
    args.entry("weekdays").or_insert(json!([]));
    serde_json::from_value(Value::Object(args)).context("Некорректные параметры расписания")
}

fn take_id(args: &mut Map<String, Value>) -> Result<Option<String>> {
    args.remove("id")
        .map(|value| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .context("Нужен непустой id расписания")
        })
        .transpose()
}

pub async fn stop_active(service: &Scheduler, duid: &str) -> Result<Option<Schedule>> {
    match service
        .list_all(duid)
        .await?
        .into_iter()
        .find(|item| item.active_run_id.is_some())
    {
        Some(item) => Ok(Some(service.stop(duid, &item.id).await?)),
        None => Ok(None),
    }
}
