use super::{
    model::{Action, Condition, Document, Run, Trigger},
    AutomationState,
};
use crate::connections::StationCommand;
use chrono::{Datelike, Timelike, Utc};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

pub(super) fn start_scheduler(state: Arc<AutomationState>) {
    tokio::spawn(async move {
        let mut connected = HashSet::new();
        let mut claimed = HashMap::<String, String>::new();
        let mut recovered = HashSet::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let Ok(stations) = state.api.storage.list().await else {
                continue;
            };
            for station in stations {
                let duid = &station.duid;
                if recovered.insert(duid.clone()) {
                    if let Ok(history) = state.history(duid).await {
                        for mut run in history.into_iter().filter(|r| r.status == "running") {
                            run.status = "interrupted".into();
                            run.finished_at = Some(Utc::now().to_rfc3339());
                            run.steps.push(
                                "Сервер перезапущен; сценарий не возобновлён автоматически".into(),
                            );
                            let _ = state.record(duid, &run).await;
                        }
                    }
                }
                let is_connected = state.api.registry.is_connected(duid);
                let first_connection = is_connected && connected.insert(duid.clone());
                if !is_connected {
                    connected.remove(duid);
                }
                if !state.api.storage.is_persistent() {
                    continue;
                }
                let Ok(document) = state.document(duid).await else {
                    continue;
                };
                for scenario in document.scenarios.iter().filter(|s| s.enabled) {
                    let Ok(zone) = scenario.timezone.parse::<chrono_tz::Tz>() else {
                        continue;
                    };
                    let now = Utc::now().with_timezone(&zone);
                    let minute = format!("{:02}:{:02}", now.hour(), now.minute());
                    let due = scenario.trigger == Trigger::Connected && first_connection
                        || scenario.trigger == Trigger::Time
                            && scenario.time == minute
                            && scenario
                                .weekdays
                                .contains(&(now.weekday().number_from_monday() as u8));
                    if !due {
                        continue;
                    }
                    if scenario.trigger == Trigger::Time {
                        let key = format!("automation-occurrence:v1:{duid}:{}", scenario.id);
                        let occurrence = format!("{}:{minute}", now.date_naive());
                        if claimed.get(&key) == Some(&occurrence) {
                            continue;
                        }
                        match state.api.storage.get_control_value(&key).await {
                            Ok(Some(value))
                                if value["occurrence"].as_str() == Some(&occurrence) =>
                            {
                                claimed.insert(key, occurrence);
                                continue;
                            }
                            Err(_) => continue,
                            _ => {}
                        }
                        // Persist before dispatch: a restart cannot play a scheduled action twice.
                        if state
                            .api
                            .storage
                            .set_control_value(&key, &json!({"occurrence":occurrence}))
                            .await
                            .is_err()
                        {
                            continue;
                        }
                        claimed.insert(key, occurrence);
                    }
                    if let Err(error) =
                        launch(state.clone(), duid.clone(), scenario.id.clone()).await
                    {
                        let run = Run {
                            id: uuid::Uuid::new_v4().to_string(),
                            scenario_id: scenario.id.clone(),
                            name: scenario.name.clone(),
                            started_at: Utc::now().to_rfc3339(),
                            finished_at: Some(Utc::now().to_rfc3339()),
                            status: "missed".into(),
                            steps: vec![error],
                        };
                        let _ = state.record(duid, &run).await;
                    }
                }
            }
        }
    });
}

pub(super) async fn launch(
    state: Arc<AutomationState>,
    duid: String,
    scenario_id: String,
) -> Result<String, String> {
    let document = state.document(&duid).await?;
    document.validate()?;
    let scenario = document
        .scenarios
        .iter()
        .find(|s| s.id == scenario_id)
        .ok_or("Сценарий не найден")?;
    if !scenario.enabled {
        return Err("Сценарий выключен".into());
    }
    let mut running = state.running.lock().await;
    if running.contains_key(&duid) || running.len() >= 4 {
        return Err("Уже выполняется сценарий или достигнут предел четырёх запусков".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let run = Run {
        id: id.clone(),
        scenario_id: scenario_id.clone(),
        name: scenario.name.clone(),
        started_at: Utc::now().to_rfc3339(),
        finished_at: None,
        status: "running".into(),
        steps: vec![],
    };
    state.record(&duid, &run).await?;
    running.insert(duid.clone(), (id.clone(), cancel.clone()));
    drop(running);
    tokio::spawn(async move {
        let mut run = run;
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            execute(&state, &duid, &document, &scenario_id, &cancel, &mut run),
        )
        .await;
        run.status = match result {
            Ok(Ok(())) => "completed".into(),
            Ok(Err(error)) => {
                run.steps.push(error);
                if cancel.load(Ordering::Relaxed) {
                    "cancelled"
                } else {
                    "failed"
                }
                .into()
            }
            Err(_) => {
                run.steps
                    .push("Достигнут предел выполнения 10 минут".into());
                "failed".into()
            }
        };
        run.finished_at = Some(Utc::now().to_rfc3339());
        if let Err(error) = state.record(&duid, &run).await {
            tracing::error!("automation history: {error}");
        }
        state.running.lock().await.remove(&duid);
    });
    Ok(id)
}

async fn execute(
    state: &AutomationState,
    duid: &str,
    document: &Document,
    first: &str,
    cancel: &AtomicBool,
    run: &mut Run,
) -> Result<(), String> {
    // A stack avoids recursive async execution. Re-entering a node or active scenario is a cycle.
    let mut stack = vec![(first.to_owned(), 0usize, HashSet::<String>::new())];
    let mut steps = 0usize;
    while let Some((scenario_id, index, _)) = stack.last() {
        let scenario_id = scenario_id.clone();
        let index = *index;
        if cancel.load(Ordering::Relaxed) {
            return Err("Остановлено пользователем".into());
        }
        let scenario = document
            .scenarios
            .iter()
            .find(|s| s.id == scenario_id)
            .ok_or("Вложенный сценарий отсутствует")?;
        if !scenario.enabled {
            return Err(format!("Сценарий «{}» выключен", scenario.name));
        }
        let Some(node) = scenario.nodes.get(index) else {
            stack.pop();
            continue;
        };
        steps += 1;
        if steps > 128 || !stack.last_mut().unwrap().2.insert(node.id.clone()) {
            return Err("Цикл или превышен предел 128 шагов".into());
        }
        let mut branch = true;
        let mut child = None;
        match &node.action {
            Action::If { condition } => {
                let now = Utc::now().with_timezone(
                    &scenario
                        .timezone
                        .parse::<chrono_tz::Tz>()
                        .map_err(|_| "Неизвестный часовой пояс")?,
                );
                branch = match condition {
                    Condition::Connected { expected } => {
                        state.api.registry.is_connected(duid) == *expected
                    }
                    Condition::Dnd { expected } => {
                        state
                            .api
                            .registry
                            .dnd_enabled(duid)
                            .ok_or("Колонка не сообщила состояние «Не беспокоить»")?
                            .0
                            == *expected
                    }
                    Condition::Weekday { days } => {
                        days.contains(&(now.weekday().number_from_monday() as u8))
                    }
                    Condition::Time { start, end } => {
                        let value = format!("{:02}:{:02}", now.hour(), now.minute());
                        if start <= end {
                            &value >= start && &value < end
                        } else {
                            &value >= start || &value < end
                        }
                    }
                };
            }
            Action::Delay { seconds } => {
                for _ in 0..*seconds {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("Остановлено пользователем".into());
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Action::Scenario { scenario_id } => {
                if stack.len() >= 8 || stack.iter().any(|(id, _, _)| id == scenario_id) {
                    return Err("Цикл вложенных сценариев или глубина больше 8".into());
                }
                child = Some(scenario_id.clone());
            }
            Action::Stop => {
                stack.pop();
                continue;
            }
            Action::Speech { text } => {
                deliver(
                    state,
                    duid,
                    StationCommand::Speak { text: text.clone() },
                    "push-raw",
                    json!({"eventText":text}),
                    cancel,
                )
                .await?
            }
            Action::Command { text } => {
                deliver(
                    state,
                    duid,
                    StationCommand::Utterance { text: text.clone() },
                    "push-text",
                    json!({"eventText":text}),
                    cancel,
                )
                .await?
            }
            Action::Volume { percent } => {
                let body = json!({"Name":"sound_set_level","Type":"client_action","IsLedSilent":true,"Payload":{"fields":[{"key":"new_level","value":{"numberValue":u64::from(*percent).div_ceil(10)}},{"key":"new_percent_level","value":{"numberValue":percent}}]}});
                let directive = crate::api::raw_directive::from_object(&body)?;
                deliver(
                    state,
                    duid,
                    StationCommand::Directives {
                        directives: vec![directive],
                    },
                    "push-directive",
                    body,
                    cancel,
                )
                .await?;
            }
            Action::Animation {
                animation,
                duration_ms,
            } => {
                let (connection, capability) = state
                    .api
                    .registry
                    .display_capability(duid)
                    .ok_or("Нет данных о поддержке анимаций колонки")?;
                let directive = capability
                    .animation(animation, *duration_ms)
                    .map_err(str::to_owned)?;
                if !state.api.registry.send_to_current(
                    duid,
                    connection,
                    StationCommand::Directives {
                        directives: vec![directive],
                    },
                ) {
                    return Err("Колонка отключилась".into());
                }
            }
            Action::Light {
                target,
                animation,
                color,
                brightness,
                duration_ms,
            } => {
                let target_kind = if target == "logo" {
                    crate::device_access::ring::Target::Logo
                } else {
                    crate::device_access::ring::Target::Ring
                };
                let script = crate::device_access::ring::script(
                    target_kind,
                    animation,
                    color,
                    *brightness,
                    *duration_ms,
                )?;
                let target = crate::device_access::operations::resolve(&state.api, duid, None)
                    .await
                    .map_err(|_| "Нет сохранённого адреса ADB для подсветки")?;
                let (code, output) =
                    crate::device_access::operations::execute(&target, &script).await?;
                if code != 0 {
                    return Err(format!(
                        "Подсветка: {}",
                        String::from_utf8_lossy(&output)
                            .chars()
                            .take(500)
                            .collect::<String>()
                    ));
                }
            }
        }
        run.steps.push(format!(
            "{} · {} · {}",
            Utc::now().format("%H:%M:%S"),
            node.id,
            if matches!(node.action, Action::If { .. }) {
                if branch {
                    "условие: да"
                } else {
                    "условие: нет"
                }
            } else {
                "шаг выполнен; аппаратное подтверждение зависит от команды"
            }
        ));
        state.record(duid, run).await?;
        let next = if branch { &node.next } else { &node.otherwise };
        stack.last_mut().unwrap().1 = match next {
            None => index + 1,
            Some(id) if id.is_empty() => scenario.nodes.len(),
            Some(id) => scenario
                .nodes
                .iter()
                .position(|n| &n.id == id)
                .ok_or("Отсутствует шаг перехода")?,
        };
        if let Some(id) = child {
            stack.push((id, 0, HashSet::new()));
        }
    }
    Ok(())
}

async fn deliver(
    state: &AutomationState,
    duid: &str,
    command: StationCommand,
    action: &str,
    body: serde_json::Value,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let feedback = state
        .api
        .control
        .create(duid, duid, action, body)
        .await
        .map_err(|e| e.to_string())?;
    if !state.api.registry.send_one(
        duid,
        StationCommand::Tracked {
            feedback: feedback.clone(),
            command: Box::new(command),
        },
    ) {
        feedback.failed("Колонка не подключена").await;
        return Err("Колонка не подключена".into());
    }
    feedback.delivered(1).await;
    for _ in 0..720 {
        if cancel.load(Ordering::Relaxed) {
            return Err("Остановлено; уже переданная команда может завершиться на колонке".into());
        }
        if let Some(record) = state
            .api
            .storage
            .get_control_value(&format!("command:{}", feedback.id))
            .await
            .map_err(|e| e.to_string())?
        {
            match record["status"].as_str() {
                Some("completed") => return Ok(()),
                Some("failed") => {
                    return Err(record["error"]
                        .as_str()
                        .unwrap_or("Команда не выполнена")
                        .to_owned())
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("Результат команды не получен за 3 минуты".into())
}
