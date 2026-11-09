use crate::{api, state::ControlState};
use dioxus::prelude::*;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq)]
struct Editor {
    status: Signal<Option<Value>>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    output: Signal<String>,
    target: Signal<Value>,
    job: Signal<Option<String>>,
}

fn base(duid: &str) -> String {
    format!("/devices/{}/packages", api::encoded(duid))
}
fn query(target: &Value) -> String {
    target["host"]
        .as_str()
        .map(|host| {
            format!(
                "host={}&port={}",
                api::encoded(host),
                target["port"].as_u64().unwrap_or(5555)
            )
        })
        .unwrap_or_default()
}
fn saved_job(duid: &str, value: Option<&Value>) -> Option<Value> {
    let storage = web_sys::window()?.local_storage().ok()??;
    let key = format!("quasar.packages.job.{duid}");
    if let Some(value) = value {
        if value.is_null() {
            let _ = storage.remove_item(&key);
        } else {
            let _ = storage.set_item(&key, &value.to_string());
        }
        None
    } else {
        storage
            .get_item(&key)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str(&value).ok())
    }
}

async fn refresh(state: ControlState, duid: &str, mut editor: Editor) {
    let target = editor.target.peek().clone();
    match api::get::<Value>(state, &format!("{}?{}", base(duid), query(&target))).await {
        Ok(value) => {
            editor.status.set(Some(value));
            editor.error.set(None);
        }
        Err(error) => {
            editor.status.set(None);
            editor.error.set(Some(error));
        }
    }
}

async fn monitor(
    state: ControlState,
    duid: String,
    job: String,
    target: Value,
    mut editor: Editor,
) {
    editor.busy.set(true);
    editor.job.set(Some(job.clone()));
    for _ in 0..900 {
        let url = format!(
            "{}?op=job&job={}&{}",
            base(&duid),
            api::encoded(&job),
            query(&target)
        );
        match api::get::<Value>(state, &url).await {
            Ok(value) => {
                editor
                    .output
                    .set(value["text"].as_str().unwrap_or_default().into());
                if value["running"] == false {
                    if value["exit_code"] != 0 {
                        editor.error.set(Some(format!(
                            "Операция завершилась с кодом {}. Подробности в журнале.",
                            value["exit_code"]
                        )));
                    } else {
                        editor.error.set(None);
                    }
                    saved_job(&duid, Some(&Value::Null));
                    editor.job.set(None);
                    editor.busy.set(false);
                    let operation_error = editor.error.peek().clone();
                    refresh(state, &duid, editor).await;
                    if operation_error.is_some() {
                        editor.error.set(operation_error);
                    }
                    return;
                }
            }
            Err(error) => {
                editor.error.set(Some(format!(
                    "{error}. Задача может продолжаться на колонке; проверьте её состояние."
                )));
                editor.busy.set(false);
                return;
            }
        }
        gloo_timers::future::TimeoutFuture::new(2000).await;
    }
    editor.error.set(Some(
        "Операция ещё не завершилась. Её можно проверить повторно.".into(),
    ));
    editor.busy.set(false);
}

fn action(state: ControlState, duid: String, mut editor: Editor, mut body: Value) {
    if *editor.busy.peek() {
        return;
    }
    editor.busy.set(true);
    editor.error.set(None);
    editor.output.set(String::new());
    let target = editor.target.peek().clone();
    body["target"] = target.clone();
    spawn(async move {
        match api::mutate(state, "POST", &base(&duid), Some(&body)).await {
            Ok(response) => {
                match response.json::<Value>().await {
                    Ok(value) => {
                        if let Some(job) = value["job"].as_str() {
                            saved_job(&duid, Some(&json!({"job":job,"target":target})));
                            monitor(state, duid, job.into(), target, editor).await;
                        } else {
                            editor.error.set(Some("Сервер не вернул номер операции; проверьте состояние перед повтором.".into()));
                            editor.busy.set(false);
                        }
                    }
                    Err(_) => {
                        editor.error.set(Some(
                            "Ответ не удалось прочитать; проверьте состояние перед повтором."
                                .into(),
                        ));
                        editor.busy.set(false);
                    }
                }
            }
            Err(error) => {
                editor.error.set(Some(error));
                editor.busy.set(false);
            }
        }
    });
}

#[component]
pub fn PackagesPage(state: ControlState) -> Element {
    let duid = (state.selected)();
    let ip = state
        .device()
        .and_then(|device| device.ip_address)
        .unwrap_or_default();
    rsx! {
        document::Stylesheet { href: asset!("/assets/packages.css") }
        if duid.is_empty() { section { class: "card", h2 { "Пакеты YAM" } p { "Выберите колонку" } } }
        else { PackageManager { key: "{duid}", state, duid, ip } }
    }
}

#[component]
fn PackageManager(state: ControlState, duid: String, ip: String) -> Element {
    let mut host = use_signal(|| ip);
    let mut port = use_signal(|| "5555".to_owned());
    let mut filter = use_signal(String::new);
    let mut replace = use_signal(|| false);
    let editor = Editor {
        status: use_signal(|| None),
        busy: use_signal(|| false),
        error: use_signal(|| None),
        output: use_signal(String::new),
        target: use_signal(|| Value::Null),
        job: use_signal(|| None),
    };
    let initial_duid = duid.clone();
    use_future(move || {
        let duid = initial_duid.clone();
        async move {
            let mut editor = editor;
            if let Some(saved) = saved_job(&duid, None) {
                if let Some(job) = saved["job"].as_str() {
                    editor.target.set(saved["target"].clone());
                    monitor(state, duid, job.into(), saved["target"].clone(), editor).await;
                    return;
                }
            }
            editor.busy.set(true);
            refresh(state, &duid, editor).await;
            editor.busy.set(false);
        }
    });
    let data = (editor.status)().unwrap_or_default();
    let installed = data["installed"] == true;
    let owned = data["owned"] == true;
    let busy = (editor.busy)();
    let can_mutate = state.allowed() && !busy;
    let catalog = data["catalog"].as_array().cloned().unwrap_or_default();
    let packages = data["packages"].as_array().cloned().unwrap_or_default();
    let services = data["services"].as_array().cloned().unwrap_or_default();
    let platform = data["platform"].as_str().unwrap_or("не определена");
    let arch = data["arch"].as_str().unwrap_or("—");
    let refresh_duid = duid.clone();
    let resume_duid = duid.clone();
    let bootstrap_duid = duid.clone();
    let remove_duid = duid.clone();
    rsx! {
        section { class: "card settings-card packages-card",
            h2 { "Пакеты YAM" }
            p { class: "settings-intro", "Официальный каталог пакетов для колонки. Перед изменениями сохраняется резервная копия YAM. Установка выполняется на колонке и продолжается после закрытия страницы." }
            details { class: "device-connection-settings",
                summary { "Подключение ADB" }
                form { class: "packages-connect", onsubmit: move |event| {
                    event.prevent_default(); let mut editor = editor;
                    if busy { return; }
                    let target = if host().trim().is_empty() { Value::Null } else { json!({"host":host().trim(),"port":port().parse::<u16>().unwrap_or(0)}) };
                    editor.target.set(target); editor.busy.set(true); let duid = refresh_duid.clone();
                    spawn(async move { refresh(state, &duid, editor).await; editor.busy.set(false); });
                },
                    input { aria_label: "Адрес колонки для пакетов", placeholder: "Адрес колонки", value: "{host}", disabled: busy, oninput: move |event| host.set(event.value()) }
                    input { aria_label: "Порт ADB для пакетов", inputmode: "numeric", maxlength: "5", value: "{port}", disabled: busy, oninput: move |event| port.set(event.value()) }
                    button { class: "button secondary", r#type: "submit", disabled: busy, "Обновить" }
                }
            }
            p { class: "player-note", "Платформа: {platform} · Архитектура: {arch}" }
            if let Some(message) = data["message"].as_str() { p { class: "form-feedback failure", "{message}" } }
            if let Some(error) = (editor.error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
            if busy { p { class: "player-note", role: "status", "Выполняется…" } }
            if !busy && (editor.job)().is_some() {
                button { class: "button secondary", onclick: move |_| {
                    if let Some(saved) = saved_job(&resume_duid, None) { if let Some(job) = saved["job"].as_str() {
                        let duid = resume_duid.clone(); let job = job.to_owned(); let target = saved["target"].clone();
                        spawn(async move { monitor(state, duid, job, target, editor).await; });
                    } }
                }, "Проверить операцию" }
            }
            if !(editor.output)().is_empty() { pre { class: "packages-output", "{editor.output}" } }
            if !installed {
                p { "YAM не установлен. Панель установит проверенную сборку 0.2.5 в /data/yam. Существующий каталог с неизвестной установкой останется нетронутым." }
                button { class: "button secondary", disabled: !can_mutate || data["platform"].is_null(), onclick: move |_| action(state, bootstrap_duid.clone(), editor, json!({"action":"bootstrap"})), "Установить YAM" }
            } else {
                div { class: "packages-actions",
                    for (name, label) in [("doctor", "Проверить пакеты"), ("repair", "Восстановить ссылки"), ("update", "Обновить YAM")] {
                        button { class: "button secondary", disabled: !can_mutate, onclick: {
                            let duid = duid.clone(); move |_| action(state, duid.clone(), editor, json!({"action":name}))
                        }, "{label}" }
                    }
                    if data["registry"] != true {
                        button { class: "button secondary", disabled: !can_mutate, onclick: { let duid = duid.clone(); move |_| action(state, duid.clone(), editor, json!({"action":"registry"})) }, "Подключить официальный реестр" }
                    }
                }
                h3 { "Установленные пакеты" }
                if packages.is_empty() { p { class: "player-note", "Пакетов пока нет" } }
                for package in packages {
                    div { class: "packages-row", key: "{package}",
                        strong { {package["name"].as_str().unwrap_or_default()} } span { {package["version"].as_str().unwrap_or_default()} }
                        button { class: "button secondary", disabled: !can_mutate, onclick: { let duid=duid.clone(); let name=package["name"].clone(); move |_| action(state,duid.clone(),editor,json!({"action":"remove","package":name})) }, "Удалить" }
                    }
                }
                if !services.is_empty() {
                    h3 { "Службы пакетов" }
                    for service in services {
                        div { class: "packages-service", key: "{service}",
                            strong { {service["package"].as_str().unwrap_or_default()} " / " {service["name"].as_str().unwrap_or_default()} }
                            p { class: "player-note", if service["running"] == true { "Работает" } else { "Не запущена панелью" } if service["enabled"] == true { " · Автозапуск включён" } }
                            if let Some(reason) = service["reason"].as_str() { p { class: "player-note", "{reason}" } }
                            div { class: "packages-actions",
                                for (command, label, supported) in [("service_start", "Запустить",service["supported"] == true), ("service_stop", "Остановить",service["managed"] == true), (if service["enabled"] == true { "service_disable" } else { "service_enable" }, if service["enabled"] == true { "Выключить автозапуск" } else { "Автозапуск" },service["supported"] == true)] {
                                    button { class: "button secondary", disabled: !can_mutate || !supported, onclick: { let duid=duid.clone(); let id=service["id"].clone(); move |_| action(state,duid.clone(),editor,json!({"action":command,"service":id})) }, "{label}" }
                                }
                            }
                        }
                    }
                }
                h3 { "Официальный каталог" }
                input { class: "packages-filter", r#type: "search", placeholder: "Название пакета", aria_label: "Поиск пакетов", value: "{filter}", oninput: move |event| filter.set(event.value()) }
                label { class: "packages-consent", input { r#type: "checkbox", checked: replace(), oninput: move |event| replace.set(event.checked()) } "Разрешить пакету заменять конфликты и устанавливать зависимости Entware (--yes)" }
                p { class: "player-note", "Сначала можно проверить план зависимостей. Подпись и контрольные суммы проверит YAM; неподходящая архитектура будет отклонена." }
                div { class: "packages-catalog",
                    for package in catalog.into_iter().filter(|package| package["name"].as_str().unwrap_or_default().to_lowercase().contains(&filter().to_lowercase())) {
                        div { class: "packages-service", key: "{package}",
                            strong { {package["name"].as_str().unwrap_or_default()} } span { " · " {package["latest"].as_str().unwrap_or_default()} }
                            p { {package["summary"].as_str().unwrap_or_default()} }
                            if package["fits"] != true { p { class: "player-note", "Для этой платформы не заявлен" } }
                            div { class: "packages-actions",
                                for (command,label) in [("plan","План установки"),("install","Установить / обновить")] {
                                    button { class: "button secondary", disabled: !can_mutate || package["fits"] != true || data["registry"] != true, onclick: { let duid=duid.clone(); let name=package["name"].clone(); move |_| action(state,duid.clone(),editor,json!({"action":command,"package":name,"yes":replace()})) }, "{label}" }
                                }
                            }
                        }
                    }
                }
                if owned {
                    details { class: "packages-remove", summary { "Удаление менеджера" }
                        p { "Удаляет YAM и все его пакеты после резервного копирования. Службы, запущенные панелью, будут остановлены." }
                        button { class: "button secondary", disabled: !can_mutate, onclick: move |_| action(state,remove_duid.clone(),editor,json!({"action":"uninstall"})), "Удалить YAM и его пакеты" }
                    }
                }
            }
        }
    }
}
