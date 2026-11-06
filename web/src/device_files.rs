use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use super::send;
use crate::icons::Icon;

const CONFIG_SHORTCUTS: [(&str, &str); 3] = [
    ("Quasar", "/system/vendor/quasar/quasar.cfg"),
    ("Запуск сервисов", "/system/vendor/quasar/launch.json"),
    ("DNS", "/etc/resolv.conf"),
];

#[derive(Clone, Deserialize, PartialEq)]
struct Entry {
    name: String,
    path: String,
    directory: bool,
    size: u64,
}

impl Entry {
    fn icon(&self) -> &'static str {
        if self.directory {
            "folder"
        } else {
            "file"
        }
    }
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

fn file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} Б")
    } else if bytes < 1_048_576 {
        format!("{:.1} КиБ", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} МиБ", bytes as f64 / 1_048_576.0)
    }
}

#[component]
pub(super) fn FileBrowser(bridge: Signal<Option<document::Eval>>, mut data: FileState) -> Element {
    let dirty = (data.content)() != (data.original)();
    let busy = (data.busy)();
    let mut location = use_signal(|| (data.path)());
    let mut filter = use_signal(String::new);
    use_effect(move || {
        location.set((data.path)());
        filter.set(String::new());
    });
    let needle = filter().to_lowercase();
    let mut entries: Vec<_> = (data.entries)()
        .into_iter()
        .filter(|entry| entry.name.to_lowercase().contains(&needle))
        .collect();
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let breadcrumbs: Vec<_> = (data.path)()
        .split('/')
        .filter(|part| !part.is_empty())
        .scan(String::new(), |path, name| {
            path.push('/');
            path.push_str(name);
            Some((name.to_owned(), path.clone()))
        })
        .collect();
    rsx! {
        div { class: if (data.file)().is_some() { "device-files file-open" } else { "device-files" },
            form { class: "device-file-location", onsubmit: move |event| { event.prevent_default(); if busy || dirty || !location().starts_with('/') {return;} data.busy.set(true); send(bridge,json!({"type":"list","path":location()})); },
                button { class: "button secondary", r#type: "button", aria_label: "Родительская папка", title: "На уровень выше", disabled: busy || dirty || (data.path)()=="/", onclick: move |_| { data.busy.set(true); send(bridge,json!({"type":"list","path":parent(&(data.path)())})); }, "↑" }
                input { aria_label: "Путь к каталогу", value:"{location}", disabled: busy || dirty, spellcheck:"false", oninput: move |event| location.set(event.value()) }
                button { class: "button secondary", r#type: "submit", disabled: busy || dirty || !location().starts_with('/'), "Перейти" }
                button { class: "button secondary", r#type: "button", aria_label: "Обновить каталог", title: "Обновить каталог", disabled: busy || dirty, onclick: move |_| { data.busy.set(true); send(bridge,json!({"type":"list","path":(data.path)()})); }, Icon { name: "refresh", size: 16 } }
            }
            nav { class: "device-file-breadcrumbs", aria_label: "Путь к папке",
                button { disabled: busy || dirty, onclick: move |_| { data.busy.set(true); send(bridge,json!({"type":"list","path":"/"})); }, "Корень" }
                for (name, path) in breadcrumbs {
                    Icon { name: "chevron", size: 12 }
                    button { disabled: busy || dirty, onclick: move |_| { data.busy.set(true); send(bridge,json!({"type":"list","path":path})); }, "{name}" }
                }
            }
            div { class: "device-files-layout",
                div { class: "device-file-browser", id: "device-file-browser",
                    nav { class: "device-file-shortcuts", aria_label: "Важные конфиги",
                        h3 { "Важные конфиги" }
                        div { class: "device-file-shortcut-list",
                            for (label, path) in CONFIG_SHORTCUTS {
                                button { key: "{path}", title: path, class: if (data.file)().as_deref() == Some(path) { "device-file-entry selected" } else { "device-file-entry" }, aria_current: if (data.file)().as_deref() == Some(path) { "true" } else { "false" }, disabled: busy || dirty,
                                    onclick: move |_| { data.busy.set(true); data.notice.set(None); send(bridge,json!({"type":"read","path":path})); },
                                    Icon { name: "code", size: 16 }
                                    span { class: "device-file-shortcut-copy", strong { "{label}" } small { "{path}" } }
                                }
                            }
                        }
                    }
                    label { class: "device-file-filter", Icon { name: "search", size: 16 }
                        input { r#type: "search", aria_label: "Найти файл в текущей папке", placeholder: "Найти в этой папке…", value: "{filter}", oninput: move |event| filter.set(event.value()) }
                    }
                    div { class: "device-file-list-heading", span { "Файлы и папки" } span { if busy { "Загрузка…" } else { "{entries.len()} из {data.entries.read().len()}" } } }
                    ul { class:"device-file-list", aria_label:"Файлы и папки", aria_busy: busy,
                        if entries.is_empty() { li { class:"device-file-empty", if busy {"Загрузка…"} else if needle.is_empty() {"Каталог пуст"} else {"Нет файлов с таким именем"} } }
                        for entry in entries {
                            li { key:"{entry.path}",
                                button { class: if (data.file)().as_deref() == Some(entry.path.as_str()) { "device-file-entry selected" } else { "device-file-entry" }, aria_current: if (data.file)().as_deref() == Some(entry.path.as_str()) { "true" } else { "false" }, disabled: busy || dirty,
                                    onclick: move |_| {data.busy.set(true); data.notice.set(None); send(bridge,json!({"type":if entry.directory {"list"} else {"read"},"path":entry.path}));},
                                    Icon { name: entry.icon(), size: 16 }
                                    span { class:"device-file-name", "{entry.name}" }
                                    small {if entry.directory {"Папка"} else {"{file_size(entry.size)}"}}
                                }
                            }
                        }
                    }
                    if (data.truncated)() { p { class:"device-access-help", "Показаны первые 500 записей. Поиск работает по этому списку." } }
                    div { class:"device-file-upload",
                        button { class:"button secondary", disabled:busy || dirty, onclick:move |_| send(bridge,json!({"type":"choose_upload"})), Icon { name: "upload", size: 16 } "Загрузить файл" }
                        if let Some(name) = (data.upload)() {
                            span {"{name}"}
                            button { class:"button primary", disabled:busy || dirty, onclick:move |_| { data.busy.set(true); let path=format!("{}/{}",(data.path)().trim_end_matches('/'),name); send(bridge,json!({"type":"upload","path":path})); }, "Загрузить в этот каталог" }
                            small {"Одноимённый файл будет заменён."}
                        }
                    }
                }
                FilePreview { bridge, data }
            }
            if let Some(message) = (data.notice)() {p {class:"form-feedback success", role:"status", "{message}"}}
            p {class:"device-access-help", "Загрузка и редактирование — до 256 КиБ. Изменения сохраняются непосредственно на колонке."}
        }
    }
}

#[component]
fn FilePreview(bridge: Signal<Option<document::Eval>>, mut data: FileState) -> Element {
    let mut editing = use_signal(|| false);
    let busy = (data.busy)();
    let dirty = (data.content)() != (data.original)();
    use_effect(move || {
        let file = (data.file)();
        editing.set(false);
        if file.is_some() {
            document::eval("if (matchMedia('(max-width: 980px)').matches) document.querySelector('.device-file-editor')?.scrollIntoView({block: 'start'});");
        }
    });
    rsx! {
        aside { class: "device-file-editor", aria_label: "Просмотр файла", aria_busy: busy,
            if let Some(path) = (data.file)() {
                div { class:"device-file-editor-heading",
                    div { strong { "{path.rsplit('/').next().unwrap_or(&path)}" } small { "{path}" } }
                    span { "{file_size((data.size)())}" }
                }
                div { class: "device-file-preview-toolbar",
                    span { class: "subtle-pill", if dirty { "Не сохранено" } else if editing() { "Редактирование" } else { "Просмотр" } }
                    a { class: "text-button device-file-back", href: "#device-file-browser", "К списку файлов" }
                    button { class: "text-button", disabled: busy || dirty, onclick: move |_| { data.file.set(None); data.content.set(None); data.original.set(None); data.bytes.set(String::new()); }, "Закрыть файл" }
                }
                div {class:"device-file-actions",
                    if (data.content)().is_some() {
                        if editing() {
                            button {class:"button primary", disabled:busy || !dirty || (data.content)().is_some_and(|value|value.len()>262_144), onclick:move |_| {data.busy.set(true); send(bridge,json!({"type":"save_text","path":(data.file)(),"text":(data.content)()}));}, "Сохранить на колонке"}
                            button {class:"button secondary", disabled:busy, onclick:move |_| { data.content.set(data.original.peek().clone()); data.notice.set(None); editing.set(false); }, if dirty { "Отменить изменения" } else { "Завершить редактирование" } }
                        } else {
                            button {class:"button primary", disabled:busy, onclick:move |_| editing.set(true), Icon { name: "edit", size: 16 } "Редактировать"}
                        }
                    }
                    button {class:"button secondary", disabled:busy || dirty, onclick:move |_| send(bridge,json!({"type":"download","name":path.rsplit('/').next().unwrap_or("file"),"data":(data.bytes)()})), "Скачать"}
                }
                if let Some(content) = (data.content)() {
                    textarea { class:"device-file-text", aria_label:"Содержимое файла", value:"{content}", wrap:"off", spellcheck:"false", readonly: !editing(), disabled:busy, oninput:move |event| { if editing() { data.content.set(Some(event.value())); data.notice.set(None); } } }
                } else { p {class:"device-access-help", "Двоичный файл. Доступно скачивание."} }
                if dirty {p {class:"device-access-help", "Есть несохранённые изменения. Сохраните или отмените их, чтобы открыть другой файл."}}
                if (data.content)().is_some_and(|value|value.len()>262_144) {p {class:"form-feedback failure", "Файл больше 256 КиБ. Уменьшите содержимое перед сохранением."}}
            } else {
                div { class: "device-file-placeholder",
                    Icon { name: "file", size: 32 }
                    strong { "Выберите файл" }
                    p { class: "device-access-help", "Содержимое откроется здесь. Для изменения текста включите редактирование." }
                }
            }
        }
    }
}
