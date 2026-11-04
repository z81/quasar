//! Voice commands operate on the same durable timers as the control UI.

use std::collections::HashMap;

use anyhow::{bail, ensure, Context, Result};
use chrono::Utc;
use serde_json::{json, Map, Value};

use crate::agent::{FunctionDefinition, FunctionKind};
use crate::connections::ConnectionRegistry;
use crate::schedules::{CreateTimer, Schedule, Scheduler};

pub fn definitions() -> HashMap<String, FunctionDefinition> {
    let mut functions = HashMap::new();
    let mut add = |name: &str, description: &str, schema: Value| {
        functions.insert(name.to_owned(), FunctionDefinition {
            description: format!("{description} Execute immediately, NEVER use the function-call schedule field. Set can_cache=false. Confirm success only after the result; explain errors. The server and station must be online for the signal. An enqueued signal is not confirmation that sound played."),
            kind: FunctionKind::Remote { has_response: true, directive: None },
            arguments_json_schema: Some(schema),
        });
    };
    add("create_timer", "Start a persistent countdown on this station, from one second to seven days. Convert the requested duration to whole seconds (ten minutes = 600). The optional title names this timer. Use this for timers, not create_schedule.", json!({
        "type":"object", "properties":{
            "title":{"type":"string","maxLength":120},
            "duration_seconds":{"type":"integer","minimum":1,"maximum":604800}
        }, "required":["duration_seconds"], "additionalProperties":false
    }));
    add("list_timers", "List this station's timers with exact ids, statuses and remaining_seconds, plus server_time. Always use this to identify an existing timer. Never invent ids or estimate remaining time from conversation history.", json!({"type":"object","properties":{},"additionalProperties":false}));
    let id_schema = json!({"type":"object","properties":{"id":{"type":"string","minLength":1}},"additionalProperties":false});
    for (name, description) in [
        ("pause_timer", "Pause a running countdown and save its exact remaining duration. This does not stop an already ringing signal."),
        ("resume_timer", "Continue a paused timer from its saved remaining duration."),
        ("cancel_timer", "Cancel a running or paused timer. If its signal is active, also request a stop of ALL active managed alarm/timer signals on this station. It keeps the timer record for restarting."),
        ("restart_timer", "Restart the chosen timer with its original full duration. An active signal must first be stopped. Requires an exact id from list_timers."),
        ("stop_timer", "Stop an active timer signal. This stops ALL currently active managed alarm/timer signals on the station, while preserving future calendar alarms. Does not cancel a countdown that is still running."),
        ("delete_timer", "Permanently delete a chosen timer. An active signal must first be stopped. Requires an exact id from list_timers. Use cancel_timer when merely asked to cancel a countdown."),
    ] {
        let mut schema = id_schema.clone();
        let selection = if matches!(name, "restart_timer" | "delete_timer") {
            schema["required"] = json!(["id"]);
            "Identify it using list_timers and pass its exact id."
        } else {
            "Identify it using list_timers; id may be omitted only if exactly one timer qualifies. If several qualify, ask which one."
        };
        add(name, &format!("{description} {selection}"), schema);
    }
    functions
}

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "create_timer"
            | "list_timers"
            | "pause_timer"
            | "resume_timer"
            | "cancel_timer"
            | "restart_timer"
            | "stop_timer"
            | "delete_timer"
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
    if matches!(name, "create_timer" | "resume_timer" | "restart_timer") {
        ensure!(
            registry.alarm_support(duid) != Some(false),
            "Колонка не поддерживает сигнал таймера"
        );
    }
    if name == "create_timer" {
        let input: CreateTimer = serde_json::from_value(Value::Object(args))
            .context("Нужны duration_seconds (целое число секунд) и необязательный title")?;
        return Ok(json!(service.create_timer(duid, input).await?));
    }
    if name == "list_timers" {
        ensure!(args.is_empty(), "У list_timers нет аргументов");
        let now = Utc::now();
        let items: Vec<Value> = service
            .list_timers(duid)
            .await?
            .into_iter()
            .map(|item| {
                let remaining = if item.status == "paused" {
                    item.remaining_ms.unwrap_or(0)
                } else {
                    item.next_run_at
                        .as_deref()
                        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                        .map_or(0, |at| {
                            at.signed_duration_since(now).num_milliseconds().max(0)
                        })
                };
                let mut value = json!(item);
                value["remaining_seconds"] = json!((remaining + 999) / 1000);
                value
            })
            .collect();
        return Ok(json!({"items":items,"server_time":now.to_rfc3339()}));
    }
    let id = args
        .remove("id")
        .map(|id| {
            id.as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .context("Нужен непустой id таймера из list_timers")
        })
        .transpose()?;
    ensure!(args.is_empty(), "Неизвестный аргумент");
    ensure!(
        !matches!(name, "restart_timer" | "delete_timer") || id.is_some(),
        "Нужен id таймера из list_timers"
    );
    let items = service.list_timers(duid).await?;
    let selected = select(&items, id.as_deref(), name)?;
    let id = &selected.id;
    let item = match name {
        "pause_timer" => service.pause_timer(duid, id).await?,
        "resume_timer" => service.resume_timer(duid, id).await?,
        "cancel_timer" => service.cancel_timer(duid, id).await?,
        "restart_timer" => service.restart_timer(duid, id).await?,
        "stop_timer" => service.stop(duid, id).await?,
        "delete_timer" => {
            service.delete_timer(duid, id).await?;
            return Ok(json!({"deleted":id}));
        }
        _ => bail!("Неизвестная функция таймера"),
    };
    if name == "stop_timer" || (name == "cancel_timer" && item.status == "stopped") {
        Ok(json!({"timer":item,"stops_station_alarm_channel":true,"physical_stop_confirmed":false}))
    } else {
        Ok(json!(item))
    }
}

fn select<'a>(items: &'a [Schedule], id: Option<&str>, action: &str) -> Result<&'a Schedule> {
    if let Some(id) = id {
        return items
            .iter()
            .find(|item| item.id == id)
            .context("Таймер не найден");
    }
    let mut matching = items.iter().filter(|item| match action {
        "pause_timer" => item.status == "scheduled" && item.enabled,
        "resume_timer" => item.status == "paused",
        "cancel_timer" => item.enabled || item.status == "paused" || item.active_run_id.is_some(),
        "stop_timer" => item.active_run_id.is_some(),
        _ => false,
    });
    let first = matching.next().context("Нет подходящего таймера")?;
    ensure!(
        matching.next().is_none(),
        "Подходящих таймеров несколько. Уточни, какой нужен, и используй id из list_timers"
    );
    Ok(first)
}
