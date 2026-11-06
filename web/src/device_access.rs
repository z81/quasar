use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::{
    icons::Icon,
    state::{ControlState, Panel},
};

#[path = "device_files.rs"]
mod files;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AccessMode {
    Terminal,
    Files,
    Display,
}

pub(super) fn send(bridge: Signal<Option<document::Eval>>, message: Value) {
    if let Some(eval) = *bridge.peek() {
        let _ = eval.send(message);
    }
}

#[component]
pub fn DeviceAccessPage(
    state: ControlState,
    #[props(default = AccessMode::Files)] mode: AccessMode,
) -> Element {
    let device = state.device();
    rsx! {
        if device.is_none() {
            div { class: "card history-empty", Icon { name: "speaker", size: 28 }
                h3 { "Выберите колонку" }
                p { if mode == AccessMode::Display { "Редактор дисплея появится после выбора колонки в меню." } else { "Терминал и файлы появятся после выбора колонки в меню." } }
            }
        }
        for device in device {
            DeviceAccess { key: "{device.duid}:{mode:?}", state, duid: device.duid, ip_address: device.ip_address, mode }
        }
    }
}

#[component]
pub fn DeviceAccess(
    mut state: ControlState,
    duid: String,
    ip_address: Option<String>,
    mode: AccessMode,
) -> Element {
    let mut host_override = use_signal(|| None::<String>);
    let host = host_override().or(ip_address).unwrap_or_default();
    let mut port = use_signal(|| "5555".to_owned());
    let mut selected_mode = use_signal(|| mode);
    let address_open = use_hook(|| host.trim().is_empty());
    let address_fields = rsx! {
        div { class: "device-address",
            label { class: "form-field", span { "Адрес колонки" } input { value: "{host}", placeholder: "192.168.1.100", autocomplete: "off", spellcheck: "false", oninput: move |event| host_override.set(Some(event.value())) } }
            label { class: "form-field device-port", span { "Порт" } input { r#type: "text", inputmode: "numeric", value: "{port}", oninput: move |event| port.set(event.value()) } }
        }
        p { class: "device-access-help", "Подключение идёт с сервера Quasar. На колонке должен быть включён сетевой ADB (обычно порт 5555). Переключение раздела закрывает соединение." }
    };
    rsx! {
        document::Stylesheet { href: asset!("/assets/device-access.css") }
        document::Stylesheet { href: asset!("/assets/vendor/xterm-6.0.0.css") }
        section { class: if mode == AccessMode::Display { "card device-access" } else { "card device-access device-access-workspace" }, aria_label: "Доступ к колонке",
            if mode == AccessMode::Display { div { class: "device-access-heading", span { class: "card-kicker", "ДОСТУП К КОЛОНКЕ" } h2 { "Подключение к Миди" } } }
            if !state.allowed() {
                p { class: "device-access-help", "Войдите в пульт Quasar, чтобы подключиться к колонке." }
                button { class: "button secondary", onclick: move |_| state.panel.set(Panel::Access), Icon { name:"lock", size:15 } "Открыть настройки доступа" }
            } else {
                if mode == AccessMode::Display { {address_fields} } else {
                    div { class: "device-access-controls",
                        div { class: "device-access-tabs", role: "tablist", aria_label: "Режим доступа",
                            for (tab, label) in [(AccessMode::Files, "Файлы"), (AccessMode::Terminal, "Терминал")] {
                                button { role: "tab", aria_selected: selected_mode() == tab, class: if selected_mode() == tab { "active" } else { "" }, onclick: move |_| selected_mode.set(tab), "{label}" }
                            }
                        }
                        details { class: "device-connection-settings", open: address_open,
                            summary { strong { "Подключение по ADB" } span { if host.trim().is_empty() { "Укажите адрес колонки" } else { "{host}:{port}" } } }
                            {address_fields}
                        }
                    }
                }
                {rsx! { Session { key: "{duid}:{selected_mode():?}", state, duid, host: host.clone(), port: port(), mode: selected_mode() } }}
            }
        }
    }
}

#[component]
fn Session(
    state: ControlState,
    duid: String,
    host: String,
    port: String,
    mode: AccessMode,
) -> Element {
    let mut bridge = use_signal(|| None::<document::Eval>);
    let mut initialized = use_signal(|| false);
    let mut connecting = use_signal(|| false);
    let mut connected = use_signal(|| false);
    let mut shell_ready = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut target = use_signal(String::new);
    let mut display_status = use_signal(|| None::<Value>);
    let mut display_busy = use_signal(|| false);
    let mut file_state = files::FileState::new();
    let element = match mode {
        AccessMode::Terminal => "device-terminal-console",
        AccessMode::Files => "device-file-console",
        AccessMode::Display => "device-display-console",
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
    let connection_address = format!("{}:{}", host.trim(), port);
    let automatic_connection = (mode == AccessMode::Files && valid_address)
        .then(|| (host.trim().to_owned(), port.clone()));
    rsx! {
        div { class: "device-session", role: if mode == AccessMode::Display { "group" } else { "tabpanel" },
            div { class: "device-session-toolbar",
                if connected() || connecting() {
                    button { class: "button secondary", onclick: move |_| { send(bridge,json!({"type":"disconnect"})); connecting.set(false); connected.set(false); shell_ready.set(false); }, "Отключиться" }
                } else {
                    button { class: "button primary", disabled: !initialized() || !valid_address, onclick: move |_| {
                        connecting.set(true); error.set(None); shell_ready.set(false); display_status.set(None); display_busy.set(false); file_state.reset(); target.set(format!("{}:{}",host.trim(),port));
                        send(bridge,json!({"type":"connect","host":host.trim(),"port":port}));
                    }, Icon { name: "link", size: 15 } "Подключиться" }
                }
                span { class: if connected() { "device-session-state connected" } else { "device-session-state" }, role: "status",
                    if connecting() { "Подключаемся…" } else if connected() { "Подключено к {target}" } else { if valid_address { "Нет соединения · {connection_address}" } else { "Укажите адрес колонки" } }
                }
                if mode == AccessMode::Terminal && connected() {
                    button { class: "text-button", onclick: move |_| send(bridge,json!({"type":"input","data":"\u{3}"})), "Остановить команду (Ctrl+C)" }
                    button { class: "text-button", onclick: move |_| send(bridge,json!({"type":"clear"})), "Очистить экран" }
                }
            }
            if let Some(message) = error() { p { class: "form-feedback failure", role: "alert", "{message}" } }
            if mode == AccessMode::Display && !shell_ready() {
                p { class: "device-access-help", "Предпросмотр доступен без подключения. Для показа на колонке нажмите «Подключиться» выше." }
            }
            div { class: if mode == AccessMode::Terminal { "device-terminal-shell" } else { "device-terminal-shell hidden" },
                div { id: element, class: "device-terminal", aria_label: "Интерактивный терминал ADB",
                    onmounted: move |_| {
                        let config = json!({"element":element,"duid":duid,"files":mode != AccessMode::Terminal,"theme":(state.theme)().name(),"xterm":asset!("/assets/vendor/xterm-6.0.0.js").to_string(),"fit":asset!("/assets/vendor/xterm-addon-fit-0.11.0.js").to_string(),"module":asset!("/assets/device-terminal.js").to_string()});
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
                        let automatic_connection = automatic_connection.clone();
                        spawn(async move {
                            while let Ok(event) = eval.recv::<Value>().await {
                                match event["type"].as_str().unwrap_or_default() {
                                    "ready" => {
                                        initialized.set(true);
                                        if let Some((host, port)) = &automatic_connection {
                                            connecting.set(true); target.set(format!("{host}:{port}"));
                                            send(bridge,json!({"type":"connect","host":host,"port":port}));
                                        }
                                    },
                                    "connected" => {
                                        connecting.set(mode != AccessMode::Terminal); connected.set(true); error.set(None);
                                        if mode != AccessMode::Terminal { send(bridge,json!({"type":"shell"})); }
                                    },
                                    "closed" => { connecting.set(false); connected.set(false); shell_ready.set(false); display_busy.set(false); file_state.busy.set(false); },
                                    "error" => { connecting.set(false); display_busy.set(false); file_state.busy.set(false); error.set(Some(event["message"].as_str().unwrap_or("Ошибка соединения").to_owned())); display_status.set(Some(event)); },
                                    "shell_ready" => {
                                        connecting.set(false); shell_ready.set(true); error.set(None);
                                        if mode == AccessMode::Files { file_state.busy.set(true); send(bridge,json!({"type":"list","path":"/"})); }
                                    },
                                    name if name.starts_with("display") || name.starts_with("ring_") => display_status.set(Some(event)),
                                    "written" => { file_state.receive(&event); file_state.busy.set(true); send(bridge,json!({"type":"list","path":file_state.current_path()})); },
                                    _ => file_state.receive(&event),
                                }
                            }
                        });
                    }
                }
            }
            match mode {
                AccessMode::Display => rsx! {
                    crate::ring_light::RingLight { bridge, ready: shell_ready, status: display_status, busy: display_busy }
                    crate::ring_light::RingLight { bridge, ready: shell_ready, status: display_status, busy: display_busy, logo: true }
                    crate::midi_display::MidiDisplay { bridge, ready: shell_ready, status: display_status, busy: display_busy }
                },
                AccessMode::Files if shell_ready() => rsx! { files::FileBrowser { bridge, data: file_state } },
                _ => rsx! {},
            }
        }
    }
}
