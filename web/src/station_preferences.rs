use crate::{api, icons::Icon, state::{ControlState, Panel}};
use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Deserialize, PartialEq)]
struct Field {
    id: String,
    group: String,
    label: String,
    hint: String,
    kind: String,
    value: Value,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Snapshot {
    fields: Vec<Field>,
    #[serde(default)] backups: Vec<Value>,
    persistent: bool,
    #[serde(default)] info: Value,
    message: Option<String>,
    warning: Option<String>,
    backup_path: Option<String>,
}
#[derive(Clone, Copy)]
struct Group { id: &'static str, section: &'static str, title: &'static str, hint: &'static str }
const GROUPS: &[Group] = &[
    Group { id: "standby", section: "display", title: "Подсветка и режим ожидания", hint: "Поведение колонки после простоя." },
    Group { id: "playback", section: "sound", title: "Переходы между треками", hint: "Параметры штатного музыкального плеера." },
    Group { id: "earcons", section: "sound", title: "Сигналы Алисы", hint: "Отдельные звуки начала, завершения и ошибок распознавания." },
    Group { id: "alarms", section: "sound", title: "Громкость будильников", hint: "Настройки штатного alarmd; серверные таймеры и собственные сценарии могут использовать отдельную громкость." },
    Group { id: "buffer", section: "sound", title: "Буфер и кэш музыки", hint: "Текущие параметры audioclient, без сброса существующей настройки буфера." },
    Group { id: "volume", section: "sound", title: "Подстройка громкости", hint: "Оценка окружающего шума и правила восстановления уровня." },
    Group { id: "microphones", section: "microphones", title: "Микрофоны", hint: "Определение речи и диагностика каналов." },
    Group { id: "recognition", section: "microphones", title: "Распознавание", hint: "Штатная обработка аудио на колонке." },
    Group { id: "calls", section: "microphones", title: "Звук звонков", hint: "Усиление микрофона для штатных звонков. Работа самих звонков зависит от доступности сервисов." },
    Group { id: "privacy", section: "system", title: "История и диагностика", hint: "Отдельные разрешения прошивки. Состояние запуска служб показано ниже." },
    Group { id: "buttons", section: "system", title: "Физические кнопки", hint: "Поддержка штатной блокировки нажатий." },
    Group { id: "network", section: "system", title: "DNS", hint: "Пользовательские параметры резолвера unbound." },
    Group { id: "access", section: "system", title: "Доступ по ADB", hint: "Настройки разрешений. Действующее подключение и запуск службы могут задаваться отдельно в прошивке." },
];

fn display_value(field: &Field) -> String {
    match field.kind.as_str() {
        "addresses" => field.value.as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default(),
        "number" => field.value.as_f64().map(|n| if n.fract() == 0. { format!("{n:.0}") } else { n.to_string() }).unwrap_or_default(),
        _ => field.value.as_str().map(str::to_owned).unwrap_or_else(|| field.value.to_string()),
    }
}
fn initial(fields: &[Field]) -> BTreeMap<String, String> {
    fields.iter().map(|f| (f.id.clone(), display_value(f))).collect()
}
fn parse(field: &Field, text: &str) -> Result<Value, String> {
    match field.kind.as_str() {
        "toggle" => match text { "true" => Ok(json!(true)), "false" => Ok(json!(false)), _ => Err("Выберите состояние".into()) },
        "number" => {
            let min = field.min.unwrap_or(0.); let max = field.max.unwrap_or(100.); let step = field.step.unwrap_or(1.);
            let message = format!("От {min} до {max}, шаг {step}");
            if text.len() > 32 || text.is_empty() || !text.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-')) { return Err(message); }
            let n = text.replace(',', ".").parse::<f64>().map_err(|_| message.clone())?;
            if !n.is_finite() || n < min || n > max || ((n / step).round() - n / step).abs() > 0.000001 { return Err(message); }
            Ok(json!((n * 1_000_000.).round() / 1_000_000.))
        }
        "addresses" => {
            let items = text.split_whitespace().collect::<Vec<_>>();
            if items.is_empty() || items.len() > 8 || items.iter().any(|s| s.parse::<std::net::IpAddr>().map_or(true, |ip| ip.is_unspecified() || ip.is_multicast())) { return Err("От 1 до 8 адресов IPv4 или IPv6, каждый с новой строки".into()); }
            Ok(json!(items))
        }
        _ => Err("Только просмотр".into()),
    }
}
fn changes(fields: &[Field], draft: &BTreeMap<String, String>) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    for field in fields.iter().filter(|f| f.kind != "readonly" && !f.value.is_null()) {
        let text = draft.get(&field.id).map(String::as_str).unwrap_or_default();
        if text == display_value(field) { continue; }
        let value = parse(field, text).map_err(|e| format!("{}: {e}", field.label))?;
        let same = match (value.as_f64(), field.value.as_f64()) { (Some(a), Some(b)) => a == b, _ => value == field.value };
        if !same { result.push(json!({"id":field.id,"previous":field.value,"value":value})); }
    }
    // Validate dependent fields before making a request; the server checks them again.
    let number = |id: &str| -> Option<f64> { let f = fields.iter().find(|f| f.id == id)?; parse(f, draft.get(id)?).ok()?.as_f64() };
    if matches!((number("alarm_start"), number("alarm_finish")), (Some(a),Some(b)) if a > b) { return Err("Конечная громкость должна быть не ниже начальной".into()); }
    if matches!((number("buffer_low"), number("buffer_high")), (Some(a),Some(b)) if a >= b) { return Err("Нижний порог буфера должен быть меньше верхнего".into()); }
    Ok(result)
}

#[component]
pub fn StationPreferences(state: ControlState, duid: String, section: String) -> Element {
    rsx! {
        document::Stylesheet { href: asset!("/assets/station-preferences.css") }
        for scope in ["user", "firmware"] {
            PreferenceSource { key: "{scope}", state, duid: duid.clone(), scope, section: section.clone() }
        }
    }
}

#[component]
fn PreferenceSource(state: ControlState, duid: String, scope: &'static str, section: String) -> Element {
    let mut snapshot = use_signal(|| None::<Snapshot>);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| true);
    let saving = use_signal(|| false);
    let mut reload = use_signal(|| 0u32);
    let url = format!("/devices/{}/preferences/{scope}", api::encoded(&duid));
    let load_url = url.clone();
    let mut task = use_future(move || {
        let url = load_url.clone();
        async move {
            loading.set(true); error.set(None);
            match api::get::<Snapshot>(state, &url).await {
                Ok(data) => snapshot.set(Some(data)),
                Err(e) => error.set(Some(e)),
            }
            loading.set(false);
        }
    });
    use_effect(move || { if reload() > 0 { task.restart(); } });
    let visible = section != "general" && (scope == "user" || section != "display");
    let source_label = if scope == "firmware" { "Настройки из quasar.cfg" } else { "Пользовательские настройки" };
    rsx! {
        div { class: "preferences-source", hidden: !visible,
            if loading() { p { class: "preferences-loading", role: "status", span { class: "spinner" } "Читаем {source_label}…" } }
            if let Some(e) = error() {
                div { class: "card preferences-error", role: "alert",
                    strong { "{source_label} недоступны" } p { "{e}" }
                    if scope == "firmware" { p { "Для чтения нужен доступ по ADB к выбранной колонке." } }
                    button { class: "button secondary", disabled: loading() || saving(), onclick: move |_| reload += 1, "Повторить чтение" }
                }
            }
            if let Some(data) = snapshot() {
                if !data.persistent && scope == "user" { p { class: "notice error", "База данных недоступна: пользовательские настройки и быстрый откат не переживут перезапуск сервера." } }
                for group in GROUPS.iter().filter(|g| data.fields.iter().any(|f| f.group == g.id)) {
                    div { key: "{group.id}", hidden: group.section != section,
                        PreferenceForm { state, group_id: group.id, title: group.title, hint: group.hint, scope, url: url.clone(),
                            fields: data.fields.iter().filter(|f| f.group == group.id).cloned().collect::<Vec<_>>(),
                            backup: data.backups.iter().find(|b| b["group"] == group.id).cloned(), snapshot, saving, reload, loading: loading() }
                    }
                }
                if scope == "firmware" {
                    div { hidden: section != "system", FirmwareInfo { state, info: data.info.clone() } }
                }
            }
        }
    }
}

#[component]
fn PreferenceForm(state: ControlState, group_id: &'static str, title: &'static str, hint: &'static str, scope: &'static str, url: String,
    fields: Vec<Field>, backup: Option<Value>, mut snapshot: Signal<Option<Snapshot>>, mut saving: Signal<bool>, mut reload: Signal<u32>, loading: bool) -> Element {
    let mut baseline = use_signal(|| fields.clone());
    let mut draft = use_signal(|| initial(&fields));
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let mut backup_path = use_signal(|| None::<String>);
    use_effect(use_reactive((&fields,), move |(fields,)| {
        if !saving() && *draft.peek() == initial(&baseline.peek()) {
            baseline.set(fields.clone()); draft.set(initial(&fields));
        }
    }));
    let form_fields = baseline();
    let pending = changes(&form_fields, &draft());
    let dirty = draft() != initial(&form_fields);
    let validation = pending.as_ref().err().cloned();
    let can_save = pending.as_ref().is_ok_and(|v| !v.is_empty()) && !saving() && !loading && state.allowed();
    let available = form_fields.iter().any(|f| f.kind != "readonly" && !f.value.is_null());
    let submit = use_callback(move |payload: Vec<Value>| {
        if saving() || loading || !state.allowed() { return; }
        saving.set(true); feedback.set(None); backup_path.set(None);
        let url = url.clone();
        dioxus::core::spawn_forever(async move {
            let result = match api::mutate(state, "PATCH", &url, Some(&json!({"group":group_id,"changes":payload}))).await {
                Ok(response) => response.json::<Snapshot>().await.map_err(|_| "Не удалось прочитать результат. Перечитайте настройки перед повтором.".into()),
                Err(e) => Err(e),
            };
            if draft.try_read().is_err() { return; }
            match result {
                Ok(next) => {
                    let fields = next.fields.iter().filter(|f| f.group == group_id).cloned().collect::<Vec<_>>();
                    baseline.set(fields.clone()); draft.set(initial(&fields));
                    let mut message = next.message.clone().unwrap_or_else(|| "Сохранено".into());
                    if let Some(warning) = &next.warning { message.push(' '); message.push_str(warning); }
                    feedback.set(Some((next.warning.is_none(), message))); backup_path.set(next.backup_path.clone());
                    // Metadata is read-only and does not change on this PATCH.
                    let mut next = next;
                    if next.info.is_null() { next.info = snapshot.peek().as_ref().map(|s| s.info.clone()).unwrap_or_default(); }
                    snapshot.set(Some(next));
                    api::refresh(state).await;
                }
                Err(e) => feedback.set(Some((false, e))),
            }
            if let Ok(mut busy) = saving.try_write() { *busy = false; }
        });
    });
    let save_changes = pending.unwrap_or_default();
    let undo = backup.as_ref().and_then(|b| b["undo"].as_array()).cloned().unwrap_or_default();
    let backup_date = backup.as_ref().and_then(|b| b["created_at"].as_str()).unwrap_or_default().to_owned();
    let has_undo = !undo.is_empty();
    rsx! {
        section { class: "card settings-card preference-card", "data-preference-group": "{scope}-{group_id}", aria_label: title,
            div { class: "preference-heading", div { h2 { "{title}" } p { "{hint}" } }
                span { class: "preference-source-label", if scope == "firmware" { "quasar.cfg" } else { "С сервера" } }
            }
            form { onsubmit: move |e| { e.prevent_default(); if can_save { submit.call(save_changes.clone()); } },
                fieldset { class: "preference-fields", disabled: saving() || loading || !state.allowed(),
                    for field in form_fields {
                        { let id = field.id.clone(); let input_id = format!("preference-{scope}-{id}");
                          let help_id = format!("{input_id}-help"); let error_id = format!("{input_id}-error");
                          let value = draft.read().get(&id).cloned().unwrap_or_default();
                          let missing = field.value.is_null();
                          let invalid = !missing && field.kind != "readonly" && parse(&field, &value).is_err();
                          let number_error = if invalid { parse(&field, &value).err() } else { None };
                          rsx! {
                            if field.kind == "toggle" {
                                label { class: "toggle-field", r#for: "{input_id}",
                                    span { strong { "{field.label}" } small { id: "{help_id}", if missing { "Параметр отсутствует в этой прошивке" } else { "{field.hint}" } } }
                                    input { id: "{input_id}", r#type: "checkbox", checked: value == "true", disabled: missing,
                                        aria_describedby: "{help_id}", onchange: move |e| { draft.write().insert(id.clone(), e.checked().to_string()); feedback.set(None); } }
                                }
                            } else if field.kind == "readonly" {
                                details { class: "preference-readonly",
                                    summary { "{field.label}" span { "Просмотр" } }
                                    p { "{field.hint}" }
                                    pre { if missing { "Параметр отсутствует" } else { "{value}" } }
                                }
                            } else {
                                div { class: "preference-field-row",
                                    label { r#for: "{input_id}", strong { "{field.label}" } small { id: "{help_id}", "{field.hint}" } }
                                    div { class: "preference-input",
                                        if field.kind == "addresses" {
                                            textarea { id: "{input_id}", rows: "4", maxlength: "400", spellcheck: false, value: "{value}", disabled: missing,
                                                aria_invalid: invalid, aria_describedby: "{help_id} {error_id}", oninput: move |e| { draft.write().insert(id.clone(), e.value()); feedback.set(None); } }
                                        } else {
                                            input { id: "{input_id}", r#type: "text", inputmode: "decimal", maxlength: "32", value: if missing { "—".into() } else { value.clone() }, disabled: missing,
                                                aria_invalid: invalid, aria_describedby: "{help_id} {error_id}", oninput: move |e| { draft.write().insert(id.clone(), e.value()); feedback.set(None); } }
                                        }
                                        if let Some(e) = number_error { small { class: "preference-validation", id: "{error_id}", "{e}" } }
                                        if missing { small { "Нет в прошивке" } }
                                    }
                                }
                            }
                          }
                        }
                    }
                }
                if available {
                    if let Some(e) = validation { p { class: "form-feedback failure", role: "status", "{e}" } }
                    if scope == "firmware" { p { class: "preference-apply-note", "Сохранение создаёт резервную копию. Новые параметры потребуют перезапуска Алисы или колонки." } }
                    div { class: "settings-actions",
                        button { class: "button primary", r#type: "submit", disabled: !can_save, Icon { name: "check", size: 16 } if saving() { "Сохраняем…" } else { "Сохранить" } }
                        button { class: "text-button", r#type: "button", disabled: !dirty || saving(), onclick: move |_| { draft.set(initial(&baseline())); feedback.set(None); }, "Отменить изменения" }
                    }
                }
            }
            if let Some((ok, message)) = feedback() { p { class: if ok { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
            if let Some(path) = backup_path() { p { class: "preference-backup-path", "Копия: " code { "{path}" } } }
            div { class: "preference-secondary-actions",
                button { class: "text-button", disabled: saving() || loading || dirty, onclick: move |_| reload += 1, "Перечитать значения" }
                if has_undo { button { class: "text-button", title: "Вернуть значения перед сохранением {backup_date}", disabled: saving() || loading || dirty,
                    onclick: move |_| submit.call(undo.clone()), "Вернуть предыдущие значения" } }
            }
        }
    }
}

#[component]
fn FirmwareInfo(mut state: ControlState, info: Value) -> Element {
    let services = info["services"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let dns = info["dns"].as_array().map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let telemetry = ["metricad", "metrica2d", "teleme3d", "fluent-bitd", "event_sender"].iter().filter(|name| info["services"].as_array().is_some_and(|v| v.iter().any(|s| s.as_str() == Some(**name)))).copied().collect::<Vec<_>>().join(", ");
    rsx! {
        section { class: "card settings-card preference-card", aria_label: "Системные файлы",
            h2 { "Системные файлы" }
            if info["available"] == true {
                dl { class: "preference-facts",
                    dt { "DNS от сети · resolv.conf" } dd { if dns.is_empty() { "Адреса не прочитаны" } else { "{dns}" } }
                    dt { "Запуск · launch.json" } dd { if services.is_empty() { "Список служб не прочитан" } else { "{services}" } }
                    dt { "Службы метрик в launch.json" } dd { if info["launch_read"] != true { "Не удалось проверить" } else if telemetry.is_empty() { "metricad, metrica2d, teleme3d, fluent-bitd и event_sender не указаны" } else { "{telemetry}" } }
                }
                p { class: "preference-apply-note", "Это список запуска, а не проверка работающих процессов. Другие механизмы запуска и встроенная телеметрия здесь не проверяются." }
            } else { p { "Не удалось прочитать сведения о DNS и запуске служб." } }
            p { class: "preference-apply-note", "resolv.conf формируется сетью. Аппаратные профили и состав запуска показываются для справки; ручное редактирование доступно в файлах." }
            div { class: "settings-actions",
                button { class: "button secondary", onclick: move |_| state.panel.set(Panel::DeviceAccess), Icon { name: "folder", size: 16 } "Открыть файлы" }
                button { class: "text-button", onclick: move |_| state.panel.set(Panel::ServerLogs), "Логи сервера и колонки" }
                button { class: "text-button", onclick: move |_| state.panel.set(Panel::Maintenance), "Службы и системные звуки" }
            }
        }
    }
}
