use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::{
    icons::Icon,
    state::{ControlState, Panel},
};

#[path = "device_files.rs"]
mod files;

pub(super) fn send(bridge: Signal<Option<document::Eval>>, message: Value) {
    if let Some(eval) = *bridge.peek() {
        let _ = eval.send(message);
    }
}

#[component]
pub fn DeviceAccessPage(state: ControlState) -> Element {
    let device = state.device();
    rsx! {
        if device.is_none() {
            div { class: "card history-empty", Icon { name: "speaker", size: 28 }
                h3 { "Выберите колонку" }
                p { "Терминал и файлы появятся после выбора колонки в меню." }
            }
        }
        for device in device {
            DeviceAccess { key: "{device.duid}", state, duid: device.duid }
        }
    }
}

#[component]
pub fn DeviceAccess(mut state: ControlState, duid: String) -> Element {
    let mut host = use_signal(String::new);
    let mut port = use_signal(|| "23".to_owned());
    let mut files_tab = use_signal(|| false);
    rsx! {
        document::Stylesheet { href: asset!("/assets/device-access.css") }
        document::Stylesheet { href: asset!("/assets/vendor/xterm-6.0.0.css") }
        section { class: "card device-access", aria_label: "Доступ к колонке",
            div { class: "device-access-heading", span { class: "card-kicker", "ДОСТУП К КОЛОНКЕ" } h2 { "Терминал и файлы" } }
            if !state.allowed() {
                p { class: "device-access-help", "Войдите в пульт Quasar, чтобы открыть терминал и файлы." }
                button { class: "button secondary", onclick: move |_| state.panel.set(Panel::Access), Icon { name:"lock", size:15 } "Открыть настройки доступа" }
            } else {
                div { class: "device-access-tabs", role: "tablist", aria_label: "Режим доступа",
                    for (files, label) in [(false, "Терминал"), (true, "Файлы")] {
                        button { role: "tab", aria_selected: files_tab() == files, class: if files_tab() == files { "active" } else { "" }, onclick: move |_| files_tab.set(files), "{label}" }
                    }
                }
                div { class: "device-address",
                    label { class: "form-field", span { "Адрес колонки" } input { value: "{host}", placeholder: "192.168.1.100", autocomplete: "off", spellcheck: "false", oninput: move |event| host.set(event.value()) } }
                    label { class: "form-field device-port", span { "Порт" } input { r#type: "text", inputmode: "numeric", value: "{port}", oninput: move |event| port.set(event.value()) } }
                }
                p { class: "device-access-help", "Укажите адрес и порт Telnet. Подключение идёт с сервера Quasar. Если открылась командная строка, можно работать. Переключение вкладки закрывает соединение." }
                {rsx! { Session { key: "{duid}:{files_tab()}", state, duid, host: host(), port: port(), files: files_tab() } }}
            }
        }
    }
}

#[component]
fn Session(state: ControlState, duid: String, host: String, port: String, files: bool) -> Element {
    let mut bridge = use_signal(|| None::<document::Eval>);
    let mut initialized = use_signal(|| false);
    let mut connecting = use_signal(|| false);
    let mut connected = use_signal(|| false);
    let mut shell_ready = use_signal(|| false);
    let mut shell_busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut target = use_signal(String::new);
    let mut display_status = use_signal(|| None::<Value>);
    let mut file_state = files::FileState::new();
    let element = if files {
        "device-file-console"
    } else {
        "device-terminal-console"
    };
    use_effect(move || {
        send(
            bridge,
            json!({"type":"theme","theme":(state.theme)().name()}),
        );
    });
    use_drop(move || {
        if let Ok(value) = bridge.try_read() {
            if let Some(eval) = *value {
                let _ = eval.send(json!({"type":"dispose"}));
            }
        }
    });
    let valid_address = !host.trim().is_empty() && port.parse::<u16>().is_ok_and(|port| port > 0);
    rsx! {
        div { class: "device-session", role: "tabpanel",
            div { class: "device-session-toolbar",
                if connected() || connecting() {
                    button { class: "button secondary", onclick: move |_| { send(bridge,json!({"type":"disconnect"})); connecting.set(false); connected.set(false); shell_ready.set(false); }, "Отключиться" }
                } else {
                    button { class: "button primary", disabled: !initialized() || !valid_address, onclick: move |_| {
                        connecting.set(true); error.set(None); shell_ready.set(false); file_state.reset(); target.set(format!("{}:{}",host.trim(),port));
                        send(bridge,json!({"type":"connect","host":host.trim(),"port":port}));
                    }, Icon { name: "link", size: 15 } "Подключиться" }
                }
                span { class: if connected() { "device-session-state connected" } else { "device-session-state" }, role: "status",
                    if connecting() { "Подключаемся…" } else if connected() { "Подключено к {target}" } else { "Нет соединения" }
                }
                if connected() && !shell_ready() {
                    button { class: "text-button", onclick: move |_| send(bridge,json!({"type":"input","data":"\u{3}"})), "Ctrl+C" }
                    button { class: "text-button", onclick: move |_| send(bridge,json!({"type":"clear"})), "Очистить" }
                }
            }
            if let Some(message) = error() { p { class: "form-feedback failure", role: "alert", "{message}" } }
            div { class: if shell_ready() { "device-terminal-shell hidden" } else { "device-terminal-shell" },
                div { id: element, class: "device-terminal", aria_label: "Интерактивный терминал Telnet",
                    onmounted: move |_| {
                        let config = json!({"element":element,"duid":duid,"files":files,"theme":(state.theme)().name(),"xterm":asset!("/assets/vendor/xterm-6.0.0.js").to_string(),"fit":asset!("/assets/vendor/xterm-addon-fit-0.11.0.js").to_string(),"module":asset!("/assets/device-terminal.js").to_string()});
                        let mut eval = document::eval(r#"
                            let terminal;
                            try {
                                const config = await dioxus.recv();
                                const module = await import(config.module);
                                terminal = await module.createTerminal(config, value => dioxus.send(value));
                                dioxus.send({type:'ready'});
                                while (true) {
                                    const message = await dioxus.recv();
                                    if (message.type === 'dispose') break;
                                    terminal.command(message);
                                }
                            } catch (error) { dioxus.send({type:'error',message:error.message || 'Не удалось открыть терминал.'}); }
                            finally { terminal?.dispose(); }
                        "#);
                        let _ = eval.send(config); bridge.set(Some(eval));
                        spawn(async move {
                            while let Ok(event) = eval.recv::<Value>().await {
                                match event["type"].as_str().unwrap_or_default() {
                                    "ready" => initialized.set(true),
                                    "connected" => { connecting.set(false); connected.set(true); error.set(None); },
                                    "closed" => { connecting.set(false); connected.set(false); shell_ready.set(false); shell_busy.set(false); file_state.busy.set(false); },
                                    "error" => { connecting.set(false); shell_busy.set(false); file_state.busy.set(false); error.set(Some(event["message"].as_str().unwrap_or("Ошибка соединения").to_owned())); display_status.set(Some(event)); },
                                    "shell_ready" => { shell_ready.set(true); shell_busy.set(false); error.set(None); file_state.busy.set(true); send(bridge,json!({"type":"list","path":"/"})); },
                                    name if name.starts_with("display") => display_status.set(Some(event)),
                                    "written" => { file_state.receive(&event); file_state.busy.set(true); send(bridge,json!({"type":"list","path":file_state.current_path()})); },
                                    _ => file_state.receive(&event),
                                }
                            }
                        });
                    }
                }
            }
            if files && connected() && !shell_ready() {
                div { class: "device-shell-confirm",
                    p { "Если уже видна командная строка, открывайте файлы — дополнительный вход не нужен." }
                    button { class: "button secondary", disabled: shell_busy(), onclick: move |_| { shell_busy.set(true); error.set(None); send(bridge,json!({"type":"shell"})); }, if shell_busy() { "Проверяем оболочку…" } else { "Оболочка готова — открыть файлы" } }
                }
            }
            if shell_ready() { files::FileBrowser { bridge, data: file_state } crate::midi_display::MidiDisplay { bridge, status: display_status, busy: file_state.busy } }
        }
    }
}
