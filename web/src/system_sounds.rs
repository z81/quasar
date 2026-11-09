use crate::{api, state::ControlState};
use dioxus::html::FileData;
use dioxus::prelude::*;
use gloo_net::http::Request;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq)]
struct Data {
    items: Signal<Vec<Value>>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    feedback: Signal<Option<String>>,
    revision: Signal<u64>,
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}
fn endpoint(duid: &str) -> String {
    format!("/devices/{}/system-sounds", api::encoded(duid))
}
fn query(host: &str, port: &str) -> String {
    if host.trim().is_empty() {
        String::new()
    } else {
        format!(
            "host={}&port={}",
            api::encoded(host.trim()),
            api::encoded(port)
        )
    }
}
fn file_url(duid: &str, item: &Value, options: &str) -> String {
    format!(
        "{}/{}/{}?{}",
        endpoint(duid),
        api::encoded(&text(&item["group"])),
        api::encoded(&text(&item["name"])),
        options
    )
}

async fn refresh(state: ControlState, duid: &str, options: &str, mut data: Data) {
    match api::get::<Value>(state, &format!("{}?{}", endpoint(duid), options)).await {
        Ok(value) => {
            if data.items.try_peek().is_ok() {
                data.items
                    .set(value["items"].as_array().cloned().unwrap_or_default());
            }
        }
        Err(error) => {
            if data.error.try_peek().is_ok() {
                data.error.set(Some(error));
            }
        }
    }
}

#[component]
pub fn SystemSoundsPage(state: ControlState) -> Element {
    rsx! {
        document::Link {rel:"stylesheet",href:asset!("/assets/system-sounds.css")}
        for device in state.device().into_iter(){ Sounds {key:"{device.duid}",state,duid:device.duid,address:device.ip_address.unwrap_or_default()} }
        if state.device().is_none(){p{class:"field-help","Выберите колонку для управления системными звуками."}}
    }
}

#[component]
fn Sounds(state: ControlState, duid: String, address: String) -> Element {
    let data = Data {
        items: use_signal(Vec::new),
        busy: use_signal(|| false),
        error: use_signal(|| None),
        feedback: use_signal(|| None),
        revision: use_signal(|| 0),
    };
    let mut host = use_signal(|| address);
    let mut port = use_signal(|| "5555".to_owned());
    let mut filter = use_signal(String::new);
    let mut selected = use_signal(|| None::<Value>);
    let mut upload = use_signal(|| None::<FileData>);
    let mut persistent = use_signal(|| false);
    let busy = (data.busy)();
    let load_duid = duid.clone();
    let apply_duid = duid.clone();
    let selection = selected();
    let filtered: Vec<_> = data
        .items
        .read()
        .iter()
        .filter(|item| {
            format!("{}/{}", text(&item["group"]), text(&item["name"]))
                .to_lowercase()
                .contains(&filter().to_lowercase())
        })
        .cloned()
        .collect();
    rsx! {
        section {class:"card settings-card system-sounds",
            h2 {"Системные звуки"}
            p {class:"settings-intro","Замените звук конкретного события. Исходный файл сохраняется; загруженный звук преобразуется в его кодек, частоту и число каналов. Предпрослушивание работает только в браузере."}
            details {
                summary {"Подключение ADB"}
                div {class:"sounds-fields",
                    label {class:"form-field",span{"Адрес"}input{value:host(),disabled:busy,oninput:move|e|host.set(e.value())}}
                    label {class:"form-field",span{"Порт"}input{inputmode:"numeric",value:port(),disabled:busy,oninput:move|e|port.set(e.value())}}
                }
            }
            div {class:"sounds-actions",
                button {class:"button secondary",disabled:busy,onclick:move |_|{let mut data=data;data.busy.set(true);data.error.set(None);let duid=load_duid.clone();let options=query(&host(),&port());spawn(async move{refresh(state,&duid,&options,data).await;if data.busy.try_peek().is_ok(){data.busy.set(false);}});},"Загрузить список с колонки"}
                button {class:"button secondary",disabled:busy,onclick:move |_|{let mut data=data;data.busy.set(true);data.error.set(None);let duid=apply_duid.clone();let options=query(&host(),&port());spawn(async move{let result=api::mutate(state,"POST",&format!("{}?{}",endpoint(&duid),options),None).await;if data.busy.try_peek().is_err(){return;}match result{Ok(_)=>{data.feedback.set(Some("Сохранённые подмены применены. Автозапуск восстановлен согласно настройкам каждого звука.".into()));data.revision.set(js_sys::Date::now()as u64);refresh(state,&duid,&options,data).await;},Err(e)=>data.error.set(Some(e))}data.busy.set(false);});},"Применить сохранённые подмены"}
                label {class:"form-field",span{"Поиск"}input{value:filter(),oninput:move|e|filter.set(e.value()),placeholder:"boot, timer, bluetooth…"}}
            }
            if let Some(error)=(data.error)(){p{class:"schedule-error",role:"alert","{error}"}}
            if let Some(message)=(data.feedback)(){p{class:"field-help",role:"status","{message}"}}
            div {class:"sounds-list",role:"list",
                for (item_key, item) in filtered.into_iter().map(|item|(format!("{}/{}",text(&item["group"]),text(&item["name"])),item)) {
                    button {key:"{item_key}",class:"sounds-entry",disabled:busy,aria_pressed:selection.as_ref().is_some_and(|v|v["group"]==item["group"]&&v["name"]==item["name"]),onclick:{let item=item.clone();move |_|{persistent.set(item["persistent"]==true);selected.set(Some(item.clone()));upload.set(None);}},
                        span {{format!("{}/{}",text(&item["group"]),text(&item["name"]))}}
                        small {{format!("{} КиБ",item["bytes"].as_u64().unwrap_or(0)/1024)} if item["custom"]==true {" · своя подмена"} if item["persistent"]==true {" · автозапуск"}}
                    }
                }
            }
            if let Some(item)=selection {
                div {class:"sounds-editor",
                    h3 {{format!("{}/{}",text(&item["group"]),text(&item["name"]))}}
                    div {class:"sounds-fields",
                        label {class:"form-field",span{"Текущий звук — в браузере"}audio{controls:true,preload:"none",src:file_url(&duid,&item,&format!("{}&revision={}",query(&host(),&port()),(data.revision)()))}}
                        label {class:"form-field",span{"Сохранённый оригинал — в браузере"}audio{controls:true,preload:"none",src:file_url(&duid,&item,&format!("{}&original=true&revision={}",query(&host(),&port()),(data.revision)()))}}
                    }
                    label {class:"form-field",span{"Новый WAV, MP3 или Opus: до 8 МиБ, до 120 секунд"}
                        input {r#type:"file",accept:".wav,.mp3,.opus,.ogg,audio/wav,audio/mpeg,audio/ogg",disabled:busy,onchange:move|event|{let mut data=data;let file=event.files().into_iter().next();if file.as_ref().is_some_and(|f|f.size()>8*1024*1024){upload.set(None);data.error.set(Some("Файл больше 8 МиБ".into()));}else{upload.set(file);data.error.set(None);}}}
                    }
                    label {class:"toggle-field",input{r#type:"checkbox",checked:persistent(),disabled:busy,onchange:move|e|persistent.set(e.checked())}span{"Восстанавливать эту подмену при загрузке колонки"}}
                    p {class:"field-help","Автозапуск включается только при подтверждённой цепочке загрузки. Хук выполняется после onboot: ранний стартовый звук может прозвучать до применения подмены. Снятие подмены возвращает чтение исходного файла без перезаписи прошивки."}
                    div {class:"sounds-actions",
                        button {class:"button primary",disabled:busy||upload.read().is_none(),onclick:{let item=item.clone();let duid=duid.clone();move |_|{
                            let Some(file)=upload()else{return;};let mut data=data;data.busy.set(true);data.error.set(None);data.feedback.set(None);
                            let options=query(&host(),&port());let url=file_url(&duid,&item,&format!("{options}&persistent={}",persistent()));let duid=duid.clone();
                            spawn(async move{
                                let result=async{
                                    let bytes=file.read_bytes().await.map_err(|_|"Не удалось прочитать файл".to_owned())?;
                                    if bytes.len()>8*1024*1024{return Err("Файл больше 8 МиБ".to_owned());}
                                    let request=Request::post(&url).header("Content-Type","application/octet-stream").body(js_sys::Uint8Array::from(bytes.as_ref())).map_err(|_|"Не удалось подготовить загрузку".to_owned())?;
                                    let response=request.send().await.map_err(|_|"Связь прервалась: проверьте состояние подмены перед повтором".to_owned())?;
                                    api::checked(state,response).await?.json::<Value>().await.map_err(|_|"Операция принята, но ответ не прочитан. Обновите список.".to_owned())
                                }.await;
                                if data.busy.try_peek().is_err(){return;}
                                match result{Ok(value)=>{data.feedback.set(Some(text(&value["message"])));data.revision.set(js_sys::Date::now()as u64);upload.set(None);refresh(state,&duid,&options,data).await;},Err(error)=>data.error.set(Some(error))}
                                data.busy.set(false);
                            });
                        }},"Преобразовать и заменить"}
                        button {class:"button secondary",disabled:busy,onclick:{let duid=duid.clone();move |_|{let mut data=data;data.busy.set(true);data.error.set(None);let options=query(&host(),&port());let url=file_url(&duid,&item,&options);let duid=duid.clone();spawn(async move{let result=api::mutate(state,"DELETE",&url,None).await;if data.busy.try_peek().is_err(){return;}match result{Ok(_)=>{data.feedback.set(Some("Подмена снята. Оригинал и резервные копии сохранены.".into()));data.revision.set(js_sys::Date::now()as u64);refresh(state,&duid,&options,data).await;},Err(e)=>data.error.set(Some(e))}data.busy.set(false);});}},"Вернуть оригинал"}
                    }
                }
            }
        }
    }
}
