use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

use crate::{api, state::ControlState};

#[derive(Clone, Copy, PartialEq)]
struct Data {
    status: Signal<Option<Value>>,
    history: Signal<Option<Value>>,
    bluetooth: Signal<Option<Value>>,
    error: Signal<Option<String>>,
    message: Signal<Option<String>>,
    loading: Signal<bool>,
    busy: Signal<bool>,
}

fn query(host: &str, port: &str) -> String {
    if host.trim().is_empty() {
        String::new()
    } else {
        format!(
            "?host={}&port={}",
            api::encoded(host.trim()),
            api::encoded(port)
        )
    }
}

async fn refresh(state: ControlState, duid: &str, target: &str, mut data: Data) {
    if !state.allowed() || *data.loading.peek() || *data.busy.peek() {
        return;
    }
    data.loading.set(true);
    let base = format!("/devices/{}/tools", api::encoded(duid));
    match api::get::<Value>(state, &format!("{base}/status{target}")).await {
        Ok(value) => {
            data.status.set(Some(value));
            data.error.set(None);
        }
        Err(error) => data.error.set(Some(error)),
    }
    if let Ok(history) = api::get::<Value>(state, &format!("{base}/history")).await {
        data.history.set(Some(history));
    }
    data.loading.set(false);
}

async fn bluetooth_refresh(state: ControlState, duid: &str, target: &str, mut data: Data) {
    if *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    match api::get::<Value>(
        state,
        &format!("/devices/{}/tools/bluetooth{target}", api::encoded(duid)),
    )
    .await
    {
        Ok(value) => {
            data.bluetooth.set(Some(value));
            data.message.set(None);
        }
        Err(error) => data.message.set(Some(error)),
    }
    data.busy.set(false);
}

fn target_body(host: &str, port: &str) -> Value {
    if host.trim().is_empty() {
        Value::Null
    } else {
        json!({"host":host.trim(),"port":port.parse::<u16>().unwrap_or(0)})
    }
}

async fn bluetooth_action(
    state: ControlState,
    duid: String,
    action: String,
    mac: Option<String>,
    target: Value,
    mut data: Data,
) {
    if *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    data.message.set(None);
    let result = api::mutate(
        state,
        "POST",
        &format!("/devices/{}/tools/bluetooth", api::encoded(&duid)),
        Some(&json!({"action":action,"mac":mac,"target":target})),
    )
    .await;
    match result {
        Ok(response) => match response.json::<Value>().await {
            Ok(value) => {
                data.bluetooth.set(Some(value));
                data.message
                    .set(Some("Состояние Bluetooth обновлено".into()));
            }
            Err(_) => data.message.set(Some(
                "Ответ не удалось прочитать. Обновите список устройств".into(),
            )),
        },
        Err(error) => data.message.set(Some(error)),
    }
    data.busy.set(false);
}

#[component]
pub fn DeviceToolsPage(state: ControlState) -> Element {
    let duid = (state.selected)();
    let ip = (state.devices)()
        .iter()
        .find(|device| device.duid == duid)
        .and_then(|device| device.ip_address.clone())
        .unwrap_or_default();
    rsx! {
        document::Stylesheet { href:asset!("/assets/device-tools.css") }
        if duid.is_empty() { section { class:"card empty-state", h2 { "Выберите колонку" } p { "Диагностика и Bluetooth доступны для выбранной станции." } } }
        else { DeviceTools { key:"{duid}",state,duid,ip } }
    }
}

#[component]
fn DeviceTools(state: ControlState, duid: String, ip: String) -> Element {
    let mut host = use_signal(|| ip);
    let mut port = use_signal(|| "5555".to_owned());
    let mut applied = use_signal(|| String::new());
    let mut applied_target = use_signal(|| Value::Null);
    let mut tab = use_signal(|| "health".to_owned());
    let mut data = Data {
        status: use_signal(|| None),
        history: use_signal(|| None),
        bluetooth: use_signal(|| None),
        error: use_signal(|| None),
        message: use_signal(|| None),
        loading: use_signal(|| false),
        busy: use_signal(|| false),
    };
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            loop {
                let hidden = js_sys::eval("document.hidden")
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if !hidden && tab.peek().as_str() == "health" {
                    refresh(state, &duid, &applied.peek(), data).await;
                }
                if !hidden
                    && tab.peek().as_str() == "bluetooth"
                    && data.bluetooth.peek().as_ref().is_some_and(|value| {
                        value["output"]["running"] == true || value["output"]["active"] == true
                    })
                {
                    bluetooth_refresh(state, &duid, &applied.peek(), data).await;
                }
                TimeoutFuture::new(10_000).await;
            }
        }
    });
    let status = (data.status)().unwrap_or(Value::Null);
    let history = (data.history)().unwrap_or(Value::Null);
    let bluetooth = (data.bluetooth)().unwrap_or(Value::Null);
    let valid_port = port().parse::<u16>().is_ok_and(|value| value > 0);
    rsx! {
        div { class:"device-tools",
            details { class:"card tools-connection",
                summary { "Подключение к колонке по ADB" }
                div { class:"tools-target",
                    label { class:"form-field", span { "Адрес" } input {value:"{host}",placeholder:"Адрес подключённой колонки",oninput:move |event|host.set(event.value())} }
                    label { class:"form-field", span { "Порт" } input {value:"{port}",inputmode:"numeric",oninput:move |event|port.set(event.value())} }
                    button { class:"button secondary",disabled:!valid_port||(data.loading)()||(data.busy)(),onclick:{let duid=duid.clone();move |_|{
                        let target=query(&host(),&port());applied.set(target.clone());applied_target.set(target_body(&host(),&port()));data.bluetooth.set(None);let duid=duid.clone();
                        spawn(async move {refresh(state,&duid,&target,data).await;});
                    }},"Подключиться" }
                }
            }
            div {class:"mode-tabs",role:"tablist",aria_label:"Инструменты колонки",
                for (id,label) in [("health","Состояние"),("bluetooth","Bluetooth"),("logs","Логи")] {
                    button {class:if tab()==id {"mode-tab selected"} else {"mode-tab"},role:"tab",aria_selected:tab()==id,
                        onclick:{let duid=duid.clone();move |_|{tab.set(id.to_owned());if id=="bluetooth" {let duid=duid.clone();let target=applied();spawn(async move {bluetooth_refresh(state,&duid,&target,data).await;});}}},"{label}"}
                }
            }
            if tab()=="health" {
                if let Some(error)=(data.error)() {div {class:"notice error",role:"alert","{error}"}}
                if let Some(error)=status["storage_error"].as_str() {div {class:"notice error",role:"alert","{error}"}}
                section {class:"card tools-health",
                    div {class:"card-heading",div {h2 {"Состояние колонки"} p { {text(&status["info"]["model"])} }},
                        button {class:"button secondary",disabled:(data.loading)(),onclick:{let duid=duid.clone();move |_|{let duid=duid.clone();let target=applied();spawn(async move{refresh(state,&duid,&target,data).await;});}},if (data.loading)() {"Загрузка…"}else{"Обновить"}}
                    }
                    div {class:"tools-metrics",
                        Metric {label:"Процессор",value:metric(&status["sample"]["cpu_percent"],"%")}
                        Metric {label:"Температура",value:metric(&status["sample"]["temperature_c"]," °C")}
                        Metric {label:"Память",value:capacity(&status["sample"]["memory_used_mb"],&status["sample"]["memory_total_mb"])}
                        Metric {label:"Диск /data",value:capacity(&status["sample"]["disk_used_mb"],&status["sample"]["disk_total_mb"])}
                        Metric {label:"Wi-Fi",value:metric(&status["sample"]["wifi_dbm"]," dBm")}
                        Metric {label:"Аптайм",value:uptime(&status["sample"]["uptime_seconds"])}
                    }
                    p {class:"field-help", {format!("Последнее чтение: {} · Нагрузка: {}", time_label(&status["sample"]["at"]), metric(&status["sample"]["load"], ""))} }
                }
                section {class:"card tools-history",
                    div {class:"card-heading",h2 {"История за 24 часа"}
                        button {class:"button secondary",disabled:(data.busy)(),onclick:{let duid=duid.clone();let enabled=history["enabled"]==true;move |_|{
                            let duid=duid.clone();let target=applied_target();let query=applied();data.busy.set(true);
                            spawn(async move {let result=api::mutate(state,"PUT",&format!("/devices/{}/tools/monitor",api::encoded(&duid)),Some(&json!({"enabled":!enabled,"target":target}))).await;
                                data.busy.set(false);if let Err(error)=result {data.error.set(Some(error));} else {refresh(state,&duid,&query,data).await;}
                            });
                        }},if history["enabled"]==true {"Остановить сбор"}else{"Включить сбор"}}
                    }
                    p {class:"field-help","Одно измерение в минуту. История хранится на нашем сервере; отправки в Яндекс нет."}
                    if history["persistent"]==false {p {class:"notice","База недоступна: история сохранится только до перезапуска сервера."}}
                    if let Some(error)=history["last_error"].as_str() {p {class:"notice error","{error}"}}
                    HistoryChart {samples:history["samples"].as_array().cloned().unwrap_or_default()}
                    if let Some(events)=history["events"].as_array() {
                        if !events.is_empty() {
                            details {
                                summary { "События соединения и перезагрузки" }
                                ul { class: "tools-events",
                                    for event in events.iter().rev().take(20) {
                                        li { {format!("{} · {}", time_label(&event["at"]), text(&event["text"]))} }
                                    }
                                }
                            }
                        }
                    }
                }
            } else if tab()=="bluetooth" {
                section {class:"card tools-bluetooth",
                    div {class:"card-heading",h2 {"Bluetooth"},button {class:"button secondary",disabled:(data.busy)(),onclick:{let duid=duid.clone();move |_|{let duid=duid.clone();let target=applied();spawn(async move {bluetooth_refresh(state,&duid,&target,data).await;});}},"Обновить"}}
                    if let Some(message)=(data.message)() {p {class:"notice",role:"status","{message}"}}
                    if bluetooth["available"]==false {p {class:"field-help",{text(&bluetooth["reason"])}}}
                    else if bluetooth.is_null() {p {"Читаем состояние адаптера…"}}
                    else {
                        p {{text(&bluetooth["name"])} " · " if bluetooth["powered"]==true {"Включён"}else{"Выключен"}}
                        div {class:"tools-actions",
                            for (action,label) in if bluetooth["powered"]==true {vec![("power_off","Выключить Bluetooth"),("scan","Найти устройства")]}else{vec![("power_on","Включить Bluetooth")]} {
                                button {class:"button secondary",disabled:(data.busy)(),onclick:{let duid=duid.clone();move |_|{spawn(bluetooth_action(state,duid.clone(),action.into(),None,applied_target(),data));}},"{label}"}
                            }
                        }
                        if (data.busy)() {p {role:"status","Выполняем действие… Поиск занимает около 5 секунд."}}
                        if let Some(devices)=bluetooth["devices"].as_array() {
                            if devices.is_empty() {p {class:"field-help","Устройств пока нет. Для нового сопряжения включите режим подключения на устройстве и выполните поиск."}}
                            for device in devices {
                                BluetoothDevice {state,duid:duid.clone(),device:device.clone(),target:applied_target(),data}
                            }
                        }
                    }
                    if !bluetooth.is_null() {BluetoothOutput {state,duid:duid.clone(),output:bluetooth["output"].clone(),target:applied_target(),data}}
                }
            } else {Logs {state,duid:duid.clone(),target:applied(),sources:status["info"]["logs"].as_array().cloned().unwrap_or_default()}}
        }
    }
}

#[component]
fn Metric(label: &'static str, value: String) -> Element {
    rsx! {div {class:"tools-metric",span {"{label}"}strong {"{value}"}}}
}
fn text(value: &Value) -> String {
    value.as_str().unwrap_or("—").to_owned()
}
fn metric(value: &Value, suffix: &str) -> String {
    value
        .as_f64()
        .map(|value| format!("{value:.1}{suffix}"))
        .unwrap_or_else(|| "—".into())
}
fn capacity(used: &Value, total: &Value) -> String {
    used.as_f64()
        .zip(total.as_f64())
        .map(|(used, total)| format!("{used:.0} / {total:.0} МБ"))
        .unwrap_or_else(|| "—".into())
}
fn uptime(value: &Value) -> String {
    value
        .as_f64()
        .map(|seconds| {
            format!(
                "{} ч {} мин",
                seconds as u64 / 3600,
                seconds as u64 % 3600 / 60
            )
        })
        .unwrap_or_else(|| "—".into())
}
fn time_label(value: &Value) -> String {
    value
        .as_i64()
        .map(|at| {
            let date = js_sys::Date::new(&(at as f64 * 1000.0).into());
            format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
        })
        .unwrap_or_else(|| "—".into())
}

#[component]
fn HistoryChart(samples: Vec<Value>) -> Element {
    let mut metric_key = use_signal(|| "cpu_percent".to_owned());
    let choices = [
        ("cpu_percent", "Процессор, %"),
        ("temperature_c", "Температура, °C"),
        ("memory_used_mb", "Память, МБ"),
        ("disk_used_mb", "Диск, МБ"),
        ("wifi_dbm", "Wi-Fi, dBm"),
        ("load", "Нагрузка"),
    ];
    let selected = metric_key();
    let values = samples
        .iter()
        .filter_map(|sample| Some((sample["at"].as_i64()?, sample[&selected].as_f64()?)))
        .collect::<Vec<_>>();
    let min = values
        .iter()
        .map(|(_, value)| *value)
        .reduce(f64::min)
        .unwrap_or(0.0);
    let max = values
        .iter()
        .map(|(_, value)| *value)
        .reduce(f64::max)
        .unwrap_or(1.0);
    let span = (max - min).max(1.0);
    let start = values.first().map(|(time, _)| *time).unwrap_or(0);
    let end = values.last().map(|(time, _)| *time).unwrap_or(1);
    let time_span = (end - start).max(60) as f64;
    let mut paths = Vec::<String>::new();
    let mut points = String::new();
    let mut previous = None;
    for (at, value) in &values {
        if previous.is_some_and(|previous| at - previous > 180) && !points.is_empty() {
            paths.push(std::mem::take(&mut points));
        }
        let x = 12.0 + (*at - start) as f64 / time_span * 696.0;
        let y = 148.0 - (value - min) / span * 136.0;
        points.push_str(&format!("{x:.1},{y:.1} "));
        previous = Some(*at);
    }
    if !points.is_empty() {
        paths.push(points);
    }
    let isolated = paths
        .iter()
        .filter(|path| path.split_whitespace().count() == 1)
        .filter_map(|path| path.trim().split_once(','))
        .map(|(x, y)| (x.to_owned(), y.to_owned()))
        .collect::<Vec<_>>();
    rsx! {
        label {class:"form-field tools-chart-select",span {"Показатель"}select {value:"{metric_key}",onchange:move |event|metric_key.set(event.value()),for (key,label) in choices {option {value:key,"{label}"}}}}
        if values.is_empty() {p {class:"tools-empty","Измерений пока нет. Включите сбор истории — первый результат появится в течение минуты."}}
        else {
            div {class:"tools-chart",role:"img",aria_label:format!("График: от {min:.1} до {max:.1}, {} измерений",values.len()),
                div {class:"tools-chart-scale",span {"{max:.1}"}span {"{min:.1}"}}
                svg {view_box:"0 0 720 160",preserve_aspect_ratio:"none","aria-hidden":true,
                    line {x1:"12",y1:"148",x2:"708",y2:"148",class:"tools-chart-axis"}
                    for points in paths {polyline {points:"{points}",class:"tools-chart-line"}}
                    for (x, y) in isolated {circle {cx:x,cy:y,r:"3",class:"tools-chart-point"}}
                }
            }
            div {class:"tools-chart-times",span {"{time_label(&json!(start))}"}span {"{time_label(&json!(end))}"}}
            details {
                summary { "Последние измерения" }
                div { class: "tools-table-wrap",
                    table { class: "tools-table",
                        thead { tr { th { "Время" } th { "Значение" } } }
                        tbody {
                            for sample in samples.iter().rev().take(20) {
                                tr {
                                    td { {time_label(&sample["at"])} }
                                    td { {metric(&sample[&selected], "")} }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn BluetoothDevice(
    state: ControlState,
    duid: String,
    device: Value,
    target: Value,
    data: Data,
) -> Element {
    let mut confirm = use_signal(|| false);
    let mut confirm_output = use_signal(|| false);
    let paired = device["paired"] == true;
    let connected = device["connected"] == true;
    let output = (data.bluetooth)().unwrap_or(Value::Null)["output"].clone();
    let mac = text(&device["mac"]);
    let actions = if paired {
        vec![
            (
                if connected { "disconnect" } else { "connect" },
                if connected {
                    "Отключить"
                } else {
                    "Подключить"
                },
            ),
            (
                if device["trusted"] == true {
                    "untrust"
                } else {
                    "trust"
                },
                if device["trusted"] == true {
                    "Убрать доверие"
                } else {
                    "Доверять"
                },
            ),
        ]
    } else {
        vec![("pair", "Сопряжение")]
    };
    rsx! {
        article {class:"tools-bt-device",
            div {strong {{text(&device["name"])}}p {class:"field-help","{mac} · " if connected {"Подключено"}else if paired {"Сопряжено"}else{"Найдено"}
                if let Some(battery)=device["battery"].as_u64() {" · Заряд {battery}%"}
            }}
            div {class:"tools-actions",
                for (action,label) in actions {
                    button {class:"button secondary",disabled:(data.busy)(),onclick:{let duid=duid.clone();let target=target.clone();let mac=mac.clone();move |_|{spawn(bluetooth_action(state,duid.clone(),action.into(),Some(mac.clone()),target.clone(),data));}},"{label}"}
                }
                if paired {
                    if confirm() {
                        button {class:"button danger",disabled:(data.busy)(),onclick:{let duid=duid.clone();let target=target.clone();let mac=mac.clone();move |_|{confirm.set(false);spawn(bluetooth_action(state,duid.clone(),"forget".into(),Some(mac.clone()),target.clone(),data));}},"Забыть устройство"}
                        button {class:"button secondary",onclick:move |_|confirm.set(false),"Отмена"}
                    }else{button {class:"button secondary",disabled:(data.busy)(),onclick:move |_|confirm.set(true),"Забыть…"}}
                }
                if paired && device["audio_sink"]==true && output["supported"]==true && output["active"]!=true && output["running"]!=true {
                    if confirm_output() {
                        p {class:"field-help","Звук кратко прервётся для переключения. Исходные настройки будут сохранены; этот выход ещё не проверен с вашей Bluetooth-колонкой."}
                        button {class:"button",disabled:(data.busy)(),onclick:{let duid=duid.clone();let target=target.clone();let mac=mac.clone();move |_|{confirm_output.set(false);spawn(bluetooth_action(state,duid.clone(),"output_start".into(),Some(mac.clone()),target.clone(),data));}},"Сохранить копию и переключить"}
                        button {class:"button secondary",onclick:move |_|confirm_output.set(false),"Отмена"}
                    }else{button {class:"button secondary",disabled:(data.busy)(),onclick:move |_|confirm_output.set(true),"Вывести звук…"}}
                }
            }
        }
    }
}

#[component]
fn BluetoothOutput(
    state: ControlState,
    duid: String,
    output: Value,
    target: Value,
    data: Data,
) -> Element {
    let phase = output["state"].as_str().unwrap_or_default();
    let description=match phase {
        "starting"=>"Подготавливаем подключение и звуковой маршрут…",
        "active"=>"Маршрут включён. Проверьте звук на внешней колонке.",
        "failed"=>"Переключение не завершилось. Исходный маршрут восстановлен; проверьте подключение внешней колонки.",
        "restore_failed"=>"Восстановление не подтверждено. Нажмите «Вернуть встроенный динамик» и проверьте звук.",
        "disconnected"=>"Bluetooth-устройство отключилось. Звук возвращён во встроенный динамик.",
        _=>"Используется встроенный динамик.",
    };
    rsx! {div {class:"tools-output",h3 {"Звук на внешней колонке"}
        p {role:"status","{description}"}
        p {class:"field-help",{text(&output["reason"])}}
        if output["supported"]==true && output["active"]!=true && output["running"]!=true {p {class:"field-help","Найдите внешнюю колонку, выполните сопряжение и нажмите «Вывести звук» в её строке."}}
        if output["active"]==true || output["running"]==true || output["needs_restore"]==true {
            button {class:"button secondary",disabled:(data.busy)(),onclick:move |_|{spawn(bluetooth_action(state,duid.clone(),"output_stop".into(),None,target.clone(),data));},"Вернуть встроенный динамик"}
        }
    }}
}

#[component]
fn Logs(state: ControlState, duid: String, target: String, sources: Vec<Value>) -> Element {
    let mut source = use_signal(|| "dmesg".to_owned());
    let mut filter = use_signal(String::new);
    let mut lines = use_signal(|| "200".to_owned());
    let mut result = use_signal(|| None::<Value>);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);
    let valid = lines()
        .parse::<usize>()
        .is_ok_and(|value| (1..=1000).contains(&value));
    rsx! {section {class:"card tools-logs",h2 {"Логи колонки"}
        div {class:"tools-log-filter",
            label {class:"form-field",span {"Журнал"}select {value:"{source}",onchange:move |event|source.set(event.value()),option {value:"dmesg","Ядро (dmesg)"}option {value:"system","Система"}for name in sources.iter().filter_map(Value::as_str){option {value:name,"{name}"}}}}
            label {class:"form-field",span {"Фильтр текста"}input {value:"{filter}",maxlength:"128",placeholder:"Ошибка, имя службы…",oninput:move |event|filter.set(event.value())}}
            label {class:"form-field",span {"Строк"}input {value:"{lines}",inputmode:"numeric",oninput:move |event|lines.set(event.value())}}
            button {class:"button secondary",disabled:loading()||!valid,onclick:move |_|{
                let join=if target.is_empty(){"?"}else{"&"};
                let url=format!("/devices/{}/tools/logs{target}{join}source={}&lines={}&filter={}",api::encoded(&duid),api::encoded(&source()),api::encoded(&lines()),api::encoded(&filter()));
                loading.set(true);error.set(None);spawn(async move{match api::get::<Value>(state,&url).await{Ok(value)=>result.set(Some(value)),Err(message)=>error.set(Some(message))}loading.set(false);});
            },if loading(){"Читаем…"}else{"Показать"}}
        }
        p {class:"field-help","Фильтр применяется к последним выбранным строкам. Журнал читается только по кнопке."}
        if let Some(message)=error(){p {class:"notice error",role:"alert","{message}"}}
        if let Some(value)=result(){
            if value["truncated"]==true {p {class:"notice","Вывод ограничен 256 КБ. Уменьшите число строк."}}
            pre {class:"tools-log-output",tabindex:"0",aria_label:"Содержимое журнала",{value["text"].as_str().filter(|text|!text.is_empty()).unwrap_or("Подходящих строк нет")}}
        }
    }}
}
