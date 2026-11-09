mod editor;

use crate::{api, state::ControlState};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
struct Data {
    draft: Signal<Value>,
    saved: Signal<Value>,
    history: Signal<Vec<Value>>,
    runtime: Signal<Option<Value>>,
    loading: Signal<bool>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    feedback: Signal<Option<String>>,
    running: Signal<bool>,
}

fn endpoint(duid: &str) -> String {
    format!("/devices/{}/automations", api::encoded(duid))
}
fn text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            String::new()
        } else {
            value.to_string()
        }
    })
}
fn items(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
fn id() -> String {
    format!(
        "a{}-{}",
        js_sys::Date::now() as u64,
        (js_sys::Math::random() * 1_000_000.0) as u64
    )
}

#[component]
pub fn AutomationsPage(state: ControlState) -> Element {
    let devices = state.device().into_iter();
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("/assets/automations.css") }
        for device in devices { StationAutomations { key: "{device.duid}", state, duid: device.duid, timezone: device.timezone.unwrap_or_else(|| "Europe/Moscow".into()), address: device.ip_address.unwrap_or_default() } }
        if state.device().is_none() { div { class: "card settings-card", h2 { "Выберите колонку" } } }
    }
}

async fn refresh(state: ControlState, duid: &str, mut data: Data, initial: bool) {
    match api::get::<Value>(state, &endpoint(duid)).await {
        Ok(value) => {
            if data.history.try_peek().is_err() {
                return;
            }
            data.history.set(items(&value["history"]));
            data.running
                .set(value["running"].as_bool().unwrap_or(false));
            if initial {
                data.draft.set(value["document"].clone());
                data.saved.set(value["document"].clone());
            }
            if value["persistent"] == false {
                data.error.set(Some(
                    "Постоянное хранилище недоступно: сохранить расписания сейчас нельзя.".into(),
                ));
            }
        }
        Err(error) => {
            if data.error.try_peek().is_ok() {
                data.error.set(Some(error));
            }
        }
    }
    if data.loading.try_peek().is_ok() {
        data.loading.set(false);
    }
}

fn action(state: ControlState, duid: String, mut data: Data, suffix: &'static str, body: Value) {
    if *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    data.error.set(None);
    data.feedback.set(None);
    spawn(async move {
        let result = api::mutate(
            state,
            "POST",
            &format!("{}{suffix}", endpoint(&duid)),
            Some(&body),
        )
        .await;
        if data.busy.try_peek().is_err() {
            return;
        }
        match result {
            Ok(response) => {
                if suffix == "/offline" {
                    match response.json::<Value>().await {
                        Ok(value) => data.runtime.set(Some(value)),
                        Err(_) => data.error.set(Some(
                            "Не удалось прочитать состояние runtime; обновите статус.".into(),
                        )),
                    }
                }
                data.feedback.set(Some(
                    if suffix == "/run" {
                        "Сценарий запущен. Результат появится в журнале."
                    } else if suffix == "/stop" {
                        "Запрошена остановка сценария."
                    } else {
                        "Операция выполнена. Проверьте состояние локального планировщика ниже."
                    }
                    .into(),
                ));
                refresh(state, &duid, data, false).await;
            }
            Err(error) => data.error.set(Some(error)),
        }
        data.busy.set(false);
    });
}

fn save(state: ControlState, duid: String, mut data: Data) {
    let mut value = data.draft.peek().clone();
    if let Err(error) = editor::numbers(&mut value) {
        data.error.set(Some(error));
        return;
    }
    data.busy.set(true);
    data.error.set(None);
    data.feedback.set(None);
    spawn(async move {
        let result = api::mutate(state, "PUT", &endpoint(&duid), Some(&value)).await;
        if data.busy.try_peek().is_err() {
            return;
        }
        match result {
            Ok(response) => match response.json::<Value>().await {
                Ok(result) => {
                    data.draft.set(result["document"].clone());
                    data.saved.set(result["document"].clone());
                    data.feedback.set(Some("Сохранено на сервере. Для офлайн-будильников нажмите «Передать на колонку».".into()));
                }
                Err(_) => data.error.set(Some(
                    "Сохранение принято, но ответ не прочитан. Обновите страницу перед повтором."
                        .into(),
                )),
            },
            Err(error) => data.error.set(Some(error)),
        }
        data.busy.set(false);
    });
}

#[component]
fn StationAutomations(
    state: ControlState,
    duid: String,
    timezone: String,
    address: String,
) -> Element {
    let data = Data {
        draft: use_signal(|| json!({"revision":0,"scenarios":[],"alarms":[]})),
        saved: use_signal(Value::default),
        history: use_signal(Vec::new),
        runtime: use_signal(|| None),
        loading: use_signal(|| true),
        busy: use_signal(|| false),
        error: use_signal(|| None),
        feedback: use_signal(|| None),
        running: use_signal(|| false),
    };
    let mut section = use_signal(|| "scenarios");
    let initial_duid = duid.clone();
    use_future(move || {
        let duid = initial_duid.clone();
        async move {
            refresh(state, &duid, data, true).await;
            loop {
                TimeoutFuture::new(5000).await;
                if state.allowed() {
                    refresh(state, &duid, data, false).await;
                }
            }
        }
    });
    let dirty = *data.draft.read() != *data.saved.read();
    let disabled = (data.busy)() || (data.loading)();
    let scenarios = items(&data.draft.read()["scenarios"]);
    let alarms = items(&data.draft.read()["alarms"]);
    let save_duid = duid.clone();
    let stop_duid = duid.clone();
    let new_timezone = timezone.clone();
    rsx! {
        div { class: "automations-page",
            section { class: "card settings-card",
                h2 { "Сценарии и локальные будильники" }
                p { class: "settings-intro", "Сценарии выполняет сервер. Локальные будильники после передачи работают на самой колонке, даже если сервер и интернет недоступны." }
                div { class: "automation-toolbar",
                    button { class: "button secondary", onclick: move |_| section.set("scenarios"), aria_pressed: section()=="scenarios", "Сценарии" }
                    button { class: "button secondary", onclick: move |_| section.set("alarms"), aria_pressed: section()=="alarms", "Офлайн-будильники" }
                    button { class: "button primary", disabled:disabled || !dirty, onclick: move |_| save(state,save_duid.clone(),data), "Сохранить изменения" }
                }
                if dirty { p { class: "field-help", "Есть несохранённые изменения. Запуск и передача используют сохранённую версию." } }
                if let Some(error) = (data.error)() { p { class: "schedule-error", role:"alert", "{error}" } }
                if let Some(message) = (data.feedback)() { p { class: "field-help", role:"status", "{message}" } }
                if (data.loading)() { p { "Загрузка…" } }
            }
            fieldset { class:"automation-fields", disabled,
                if section()=="scenarios" {
                    section { class: "card settings-card",
                        p { class:"field-help", "Условия переходят по веткам «Да» и «Нет». Обычные шаги идут по порядку. Циклы останавливаются; предел запуска — 128 шагов, 8 вложенных сценариев и 10 минут." }
                        button { class:"button secondary", disabled:scenarios.len()>=32, onclick:move |_| { let mut draft=data.draft; draft.with_mut(|d| d["scenarios"].as_array_mut().unwrap().push(json!({"id":id(),"name":"Новый сценарий","enabled":true,"trigger":"manual","timezone":new_timezone,"time":"08:00","weekdays":[1,2,3,4,5,6,7],"nodes":[]}))); }, "Добавить сценарий" }
                        if (data.running)() { button { class:"button secondary", onclick:move |_| action(state,stop_duid.clone(),data,"/stop",json!({})), "Остановить текущий сценарий" } }
                    }
                    for (index, scenario_key) in scenarios.iter().enumerate().map(|(index,item)|(index,text(&item["id"]))) {
                        editor::ScenarioEditor { key:"{scenario_key}", state, duid:duid.clone(), data, index, dirty }
                    }
                    section { class:"card settings-card",
                        h3 { "Журнал сценариев" }
                        if data.history.read().is_empty() { p { class:"field-help", "Запусков пока нет." } }
                        for (run_key, run) in (data.history)().into_iter().map(|run|(text(&run["id"]),run)) { details { key:"{run_key}", class:"automation-history",
                            summary { {format!("{} · {} · {}",text(&run["name"]),text(&run["status"]),text(&run["started_at"]))} }
                            for line in items(&run["steps"]) { p { class:"field-help", "{text(&line)}" } }
                        } }
                    }
                } else {
                    OfflineRuntime { state, duid:duid.clone(), data, dirty, address }
                    section { class:"card settings-card",
                        button { class:"button secondary", disabled:alarms.len()>=32, onclick:move |_| { let mut draft=data.draft; draft.with_mut(|d| d["alarms"].as_array_mut().unwrap().push(json!({"id":id(),"name":"Локальный будильник","enabled":true,"time":"08:00","timezone":timezone,"weekdays":[1,2,3,4,5,6,7],"date":null,"sound":"","volume":50,"ramp_seconds":30,"snooze_minutes":5,"timeout_minutes":10}))); }, "Добавить локальный будильник" }
                    }
                    for (index, alarm_key) in alarms.iter().enumerate().map(|(index,item)|(index,text(&item["id"]))) { editor::AlarmEditor { key:"{alarm_key}", draft:data.draft, index } }
                }
            }
        }
    }
}

#[component]
fn OfflineRuntime(
    state: ControlState,
    duid: String,
    data: Data,
    dirty: bool,
    address: String,
) -> Element {
    let mut host = use_signal(|| address);
    let mut port = use_signal(|| "5555".to_owned());
    let mut pcm = use_signal(String::new);
    let mut mixer = use_signal(String::new);
    let runtime = data.runtime.read().clone();
    let status_duid = duid.clone();
    let installed = runtime
        .as_ref()
        .is_some_and(|v| v["runtime"]["installed"] == "1");
    let running = runtime
        .as_ref()
        .is_some_and(|v| v["runtime"]["running"] == "1");
    rsx! {
        section { class:"card settings-card",
            h3 { "Планировщик на колонке" }
            p { class:"field-help", "Нужен root-доступ по ADB. Установка сохраняет резервную копию файлов и добавляет отдельный хук в проверенный путь загрузки. Проверка статуса и установка не проигрывают тестовый звук; наступивший будильник сработает по расписанию." }
            details { class:"automation-connection",
                summary { "Подключение и ALSA" }
                div { class:"automation-grid",
                    editor::Field { label:"Адрес ADB", value:host(), onchange:move |v| host.set(v) }
                    editor::Field { label:"Порт", value:port(), kind:"number", onchange:move |v| port.set(v) }
                    editor::Field { label:"PCM (пусто — обнаружить plug:speakers/default)", value:pcm(), onchange:move |v| pcm.set(v) }
                    editor::Field { label:"Регулятор громкости (пусто — проверить Master)", value:mixer(), onchange:move |v| mixer.set(v) }
                }
            }
            div { class:"automation-toolbar",
                button { class:"button secondary", onclick:move |_| { let mut data=data; let url=format!("{}/offline?host={}&port={}",endpoint(&status_duid),api::encoded(&host()),api::encoded(&port())); data.busy.set(true); spawn(async move { let result=api::get::<Value>(state,&url).await; if data.busy.try_peek().is_err(){return;} match result { Ok(v)=>data.runtime.set(Some(v)),Err(e)=>data.error.set(Some(e)) } data.busy.set(false); }); }, "Проверить статус" }
                for (operation,label) in [("install","Установить и запустить"),("sync","Передать на колонку"),("start","Запустить"),("stop","Остановить звонок"),("snooze","Отложить звонок"),("shutdown","Выключить планировщик"),("uninstall","Удалить runtime")] {
                    button { class:"button secondary", disabled: (dirty && matches!(operation,"install"|"sync")) || (!installed && operation!="install"), onclick:{let duid=duid.clone(); move |_| { let mut data=data; let Ok(port)=port().parse::<u16>() else { data.error.set(Some("Порт должен быть числом от 1 до 65535".into()));return; }; action(state,duid.clone(),data,"/offline",json!({"action":operation,"target":{"host":host(),"port":port},"pcm":pcm(),"mixer":mixer()})); }}, "{label}" }
                }
            }
            if let Some(value)=runtime {
                p { role:"status", if running { "Runtime запущен" } else { "Runtime остановлен или не установлен" } }
                p { class:"field-help", {format!("Автозапуск: {} · путь загрузки: {} · версия на колонке: {} · версия на сервере: {}",text(&value["runtime"]["autostart"]),text(&value["runtime"]["boot_route"]),text(&value["runtime"]["revision"]),text(&data.saved.read()["revision"]))} }
                if value["runtime"]["active"].is_string() { p { {format!("Звонит: {}",text(&value["runtime"]["active"]))} } }
                if value["runtime"]["snoozed"].is_string() { p { {format!("Отложено: {}",text(&value["runtime"]["snoozed"]))} } }
                if value["runtime"]["error"].is_string() { p { class:"schedule-error", {text(&value["runtime"]["error"])} } }
                pre { class:"automation-log", {text(&value["log"])} }
            }
            p { class:"field-help", "Локальные команды без сервера: /data/quasar/alice-automations/control.sh stop или snooze. Аппаратные кнопки не переназначаются. Звук, громкость и часы зависят от ALSA и системного времени колонки. Удаление runtime сохраняет расписания и резервные копии." }
        }
    }
}
