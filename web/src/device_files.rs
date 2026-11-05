use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use super::send;

#[derive(Clone, Deserialize, PartialEq)]
struct Entry {
    name: String,
    path: String,
    directory: bool,
    size: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct FileState {
    path: Signal<String>,
    entries: Signal<Vec<Entry>>,
    truncated: Signal<bool>,
    pub busy: Signal<bool>,
    file: Signal<Option<String>>,
    content: Signal<Option<String>>,
    original: Signal<Option<String>>,
    bytes: Signal<String>,
    size: Signal<u64>,
    upload: Signal<Option<String>>,
    notice: Signal<Option<String>>,
}

impl FileState {
    pub fn current_path(&self) -> String {
        self.path.peek().clone()
    }
    pub fn new() -> Self {
        Self {
            path: use_signal(|| "/".to_owned()),
            entries: use_signal(Vec::new),
            truncated: use_signal(|| false),
            busy: use_signal(|| false),
            file: use_signal(|| None),
            content: use_signal(|| None),
            original: use_signal(|| None),
            bytes: use_signal(String::new),
            size: use_signal(|| 0),
            upload: use_signal(|| None),
            notice: use_signal(|| None),
        }
    }
    pub fn reset(&mut self) {
        self.path.set("/".into());
        self.entries.set(Vec::new());
        self.file.set(None);
        self.content.set(None);
        self.original.set(None);
        self.bytes.set(String::new());
        self.upload.set(None);
        self.notice.set(None);
        self.busy.set(false);
    }
    pub fn receive(&mut self, event: &Value) {
        match event["type"].as_str().unwrap_or_default() {
            "list" => {
                self.path
                    .set(event["path"].as_str().unwrap_or("/").to_owned());
                if let Ok(entries) = serde_json::from_value(event["entries"].clone()) {
                    self.entries.set(entries);
                }
                self.truncated
                    .set(event["truncated"].as_bool().unwrap_or(false));
                self.busy.set(false);
            }
            "file" => {
                self.file.set(event["path"].as_str().map(str::to_owned));
                let content = event["text"].as_str().map(str::to_owned);
                self.original.set(content.clone());
                self.content.set(content);
                self.bytes
                    .set(event["data"].as_str().unwrap_or_default().to_owned());
                self.size.set(event["size"].as_u64().unwrap_or(0));
                self.notice.set(None);
                self.busy.set(false);
            }
            "written" => {
                self.notice.set(Some("Файл сохранён на колонке.".into()));
                if event["path"].as_str() == self.file.peek().as_deref() {
                    if let Some(data) = event["data"].as_str() {
                        self.original.set(self.content.peek().clone());
                        self.bytes.set(data.to_owned());
                        if let Some(size) = event["size"].as_u64() {
                            self.size.set(size);
                        }
                    } else {
                        self.file.set(None);
                        self.content.set(None);
                        self.original.set(None);
                        self.bytes.set(String::new());
                    }
                }
                self.upload.set(None);
                self.busy.set(false);
            }
            "upload_selected" => {
                self.upload.set(event["name"].as_str().map(str::to_owned));
                self.notice.set(None);
            }
            _ => {}
        }
    }
}

fn parent(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
        .to_owned()
}

#[component]
pub(super) fn FileBrowser(bridge: Signal<Option<document::Eval>>, mut data: FileState) -> Element {
    let dirty = (data.content)() != (data.original)();
    let busy = (data.busy)();
    let mut location = use_signal(|| (data.path)());
    use_effect(move || location.set((data.path)()));
    rsx! {
        div { class: "device-files",
            div { class: "device-files-layout",
                div { class: "device-file-browser",
                    form { class: "device-file-location", onsubmit: move |event| { event.prevent_default(); if busy || dirty {return;} data.busy.set(true); send(bridge,json!({"type":"list","path":location()})); },
                        button { class: "button secondary", r#type: "button", disabled: busy || dirty || (data.path)()=="/", onclick: move |_| { data.busy.set(true); send(bridge,json!({"type":"list","path":parent(&(data.path)())})); }, "↑" }
                        input { aria_label: "Путь к каталогу", value:"{location}", disabled: busy || dirty, spellcheck:"false", oninput: move |event| location.set(event.value()) }
                        button { class: "button secondary", r#type: "submit", disabled: busy || dirty || !location().starts_with('/'), "Открыть" }
                    }
                    div { class:"device-file-list", role:"list", aria_label:"Файлы и папки",
                        if (data.entries)().is_empty() { p { class:"device-access-help", if busy {"Загрузка…"} else {"Каталог пуст"} } }
                        for entry in (data.entries)() {
                            button { key:"{entry.path}", class: if (data.file)().as_deref() == Some(entry.path.as_str()) { "device-file-entry selected" } else { "device-file-entry" }, aria_current: if (data.file)().as_deref() == Some(entry.path.as_str()) { "true" } else { "false" }, disabled: busy || dirty,
                                onclick: move |_| {data.busy.set(true); data.notice.set(None); send(bridge,json!({"type":if entry.directory {"list"} else {"read"},"path":entry.path}));},
                                span { class:"device-file-kind", aria_hidden:"true", if entry.directory {"▸"} else {"·"} }
                                span { class:"device-file-name", "{entry.name}" }
                                small {if entry.directory {"Папка"} else {"{entry.size} Б"}}
                            }
                        }
                    }
                    if (data.truncated)() { p { class:"device-access-help", "Показаны первые 500 записей." } }
                    div { class:"device-file-upload",
                        button { class:"text-button", disabled:busy || dirty, onclick:move |_| send(bridge,json!({"type":"choose_upload"})), "Выбрать файл для загрузки" }
                        if let Some(name) = (data.upload)() {
                            span {"{name}"}
                            button { class:"button secondary", disabled:busy || dirty, onclick:move |_| { data.busy.set(true); let path=format!("{}/{}",(data.path)().trim_end_matches('/'),name); send(bridge,json!({"type":"upload","path":path})); }, "Загрузить в этот каталог" }
                            small {"Одноимённый файл будет заменён."}
                        }
                    }
                }
                aside { class: "device-file-editor", aria_label: "Просмотр файла", aria_busy: busy,
                    if let Some(path) = (data.file)() {
                        div { class:"device-file-editor-heading", strong {"{path}"} span {"{(data.size)()} Б"} }
                        if let Some(content) = (data.content)() {
                            textarea { class:"device-file-text", aria_label:"Содержимое файла", value:"{content}", wrap:"off", spellcheck:"false", disabled:busy, oninput:move |event| {data.content.set(Some(event.value())); data.notice.set(None);} }
                        } else { p {class:"device-access-help", "Двоичный файл. Доступно скачивание."} }
                        div {class:"device-file-actions",
                            if (data.content)().is_some() {
                                button {class:"button primary", disabled:busy || !dirty || (data.content)().is_some_and(|value|value.len()>262_144), onclick:move |_| {data.busy.set(true); send(bridge,json!({"type":"save_text","path":(data.file)(),"text":(data.content)()}));}, "Сохранить"}
                            }
                            button {class:"button secondary", disabled:busy || dirty, onclick:move |_| send(bridge,json!({"type":"download","name":path.rsplit('/').next().unwrap_or("file"),"data":(data.bytes)()})), "Скачать"}
                            button {class:"text-button", disabled:busy, onclick:move |_| {data.file.set(None); data.content.set(None); data.original.set(None); data.bytes.set(String::new());}, if dirty {"Отменить изменения"} else {"Закрыть файл"}}
                        }
                        if dirty {p {class:"device-access-help", "Есть несохранённые изменения. Сохраните или отмените их, чтобы открыть другой файл."}}
                    } else {
                        div { class: "device-file-placeholder",
                            strong { "Просмотр файла" }
                            p { class: "device-access-help", "Выберите файл в списке, чтобы открыть его содержимое." }
                        }
                    }
                }
            }
            if let Some(message) = (data.notice)() {p {class:"form-feedback success", role:"status", "{message}"}}
            p {class:"device-access-help", "Загрузка и редактирование — до 256 КиБ. Изменения сохраняются непосредственно на колонке."}
        }
    }
}
