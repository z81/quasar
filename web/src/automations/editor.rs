use super::{action, id, items, text, Data};
use crate::state::ControlState;
use dioxus::prelude::*;
use serde_json::{json, Value};

#[component]
pub(super) fn Field(
    label: String,
    value: String,
    #[props(default = "text")] kind: &'static str,
    onchange: EventHandler<String>,
) -> Element {
    rsx! { label { class:"form-field", span { "{label}" } input { r#type:if kind=="number" {"text"} else {kind}, inputmode:if kind=="number" {"numeric"} else {"text"}, value, oninput:move |event| onchange.call(event.value()) } } }
}

fn scenario(mut draft: Signal<Value>, index: usize, key: &str, value: Value) {
    draft.with_mut(|d| d["scenarios"][index][key] = value);
}
fn alarm(mut draft: Signal<Value>, index: usize, key: &str, value: Value) {
    draft.with_mut(|d| d["alarms"][index][key] = value);
}
fn node(mut draft: Signal<Value>, s: usize, n: usize, key: &str, value: Value) {
    draft.with_mut(|d| d["scenarios"][s]["nodes"][n][key] = value);
}
fn argument(mut draft: Signal<Value>, s: usize, n: usize, key: &str, value: Value) {
    draft.with_mut(|d| d["scenarios"][s]["nodes"][n]["action"][key] = value);
}

#[component]
fn Days(value: Vec<Value>, onchange: EventHandler<Value>) -> Element {
    rsx! { div { class:"automation-days", for (day,label) in [(1,"Пн"),(2,"Вт"),(3,"Ср"),(4,"Чт"),(5,"Пт"),(6,"Сб"),(7,"Вс")] {
        label { input { r#type:"checkbox", checked:value.contains(&json!(day)), onchange:{let value=value.clone(); move |event| { let mut days=value.clone(); days.retain(|v| v!=&json!(day)); if event.checked() { days.push(json!(day)); } days.sort_by_key(|v|v.as_u64()); onchange.call(json!(days)); }} } "{label}" }
    } } }
}

#[component]
pub(super) fn ScenarioEditor(
    state: ControlState,
    duid: String,
    data: Data,
    index: usize,
    dirty: bool,
) -> Element {
    let draft = data.draft;
    let value = draft.read()["scenarios"][index].clone();
    let nodes = items(&value["nodes"]);
    let run_id = text(&value["id"]);
    let trigger = text(&value["trigger"]);
    rsx! {
        section { class:"card settings-card",
            div { class:"automation-node-header",
                h3 { {text(&value["name"])} }
                button { class:"button secondary", disabled:dirty || (data.running)(), onclick:move |_| action(state,duid.clone(),data,"/run",json!({"scenario_id":run_id})), "Запустить" }
                button { class:"text-button", onclick:move |_| { let mut draft=draft; draft.with_mut(|d| { d["scenarios"].as_array_mut().unwrap().remove(index); }); }, "Удалить" }
            }
            div { class:"automation-grid",
                Field { label:"Название", value:text(&value["name"]), onchange:move |v| scenario(draft,index,"name",json!(v)) }
                label { class:"form-field", span { "Запуск" } select { value:trigger.clone(), onchange:move |e| scenario(draft,index,"trigger",json!(e.value())), option { value:"manual", "Вручную" } option { value:"time", "По времени" } option { value:"connected", "При подключении колонки к серверу" } } }
                Field { label:"Часовой пояс IANA", value:text(&value["timezone"]), onchange:move |v| scenario(draft,index,"timezone",json!(v)) }
                if trigger=="time" { Field { label:"Время", kind:"time", value:text(&value["time"]), onchange:move |v| scenario(draft,index,"time",json!(v)) } }
            }
            label { class:"toggle-field", input { r#type:"checkbox", checked:value["enabled"]==true, onchange:move |e| scenario(draft,index,"enabled",json!(e.checked())) } span { "Сценарий включён" } }
            if trigger=="time" { Days { value:items(&value["weekdays"]), onchange:move |v| scenario(draft,index,"weekdays",v) } }
            if trigger=="connected" { p { class:"field-help", "Сработает при первом подключении после запуска сервера и после потери связи. Это событие соединения, а не доказательство перезагрузки колонки." } }
            for (n, item_key) in nodes.iter().enumerate().map(|(index,item)|(index,text(&item["id"]))) { NodeEditor { key:"{item_key}", draft, s:index, n } }
            button { class:"button secondary", disabled:nodes.len()>=64, onclick:move |_| { let mut draft=draft; draft.with_mut(|d| d["scenarios"][index]["nodes"].as_array_mut().unwrap().push(json!({"id":id(),"action":{"kind":"speech","text":""},"next":null,"otherwise":null}))); }, "Добавить шаг" }
        }
    }
}

fn default_action(kind: &str) -> Value {
    match kind {
        "speech" | "command" => json!({"kind":kind,"text":""}),
        "volume" => json!({"kind":kind,"percent":50}),
        "delay" => json!({"kind":kind,"seconds":5}),
        "animation" => json!({"kind":kind,"animation":"","duration_ms":5000}),
        "light" => {
            json!({"kind":kind,"target":"ring","animation":"pulse","color":"#8b5cf6","brightness":60,"duration_ms":5000})
        }
        "scenario" => json!({"kind":kind,"scenario_id":""}),
        "if" => json!({"kind":kind,"condition":{"kind":"connected","expected":true}}),
        _ => json!({"kind":"stop"}),
    }
}

#[component]
fn NodeEditor(draft: Signal<Value>, s: usize, n: usize) -> Element {
    let all = draft.read().clone();
    let nodes = items(&all["scenarios"][s]["nodes"]);
    let value = nodes[n].clone();
    let a = value["action"].clone();
    let kind = text(&a["kind"]);
    let node_count = nodes.len();
    let identifier = text(&value["id"]);
    rsx! {
        div { class:"automation-node",
            div { class:"automation-node-header",
                strong { "Шаг {n+1} · {identifier}" }
                button { class:"text-button", disabled:n==0, onclick:move |_| { let mut draft=draft; draft.with_mut(|d| d["scenarios"][s]["nodes"].as_array_mut().unwrap().swap(n,n-1)); }, "Выше" }
                button { class:"text-button", disabled:n+1==node_count, onclick:move |_| { let mut draft=draft; draft.with_mut(|d| d["scenarios"][s]["nodes"].as_array_mut().unwrap().swap(n,n+1)); }, "Ниже" }
                button { class:"text-button", onclick:move |_| { let mut draft=draft; draft.with_mut(|d| { let nodes=d["scenarios"][s]["nodes"].as_array_mut().unwrap(); let removed=nodes.remove(n); for node in nodes { for key in ["next","otherwise"] { if node[key]==removed["id"] { node[key]=Value::Null; } } } }); }, "Удалить шаг" }
            }
            label { class:"form-field", span { "Действие" } select { value:kind.clone(), onchange:move |e| node(draft,s,n,"action",default_action(&e.value())),
                for (value,label) in [("speech","Произнести текст"),("command","Команда ассистенту"),("volume","Громкость"),("delay","Пауза"),("light","Подсветка Миди"),("animation","Штатная анимация"),("scenario","Другой сценарий"),("if","Условие"),("stop","Завершить сценарий")] { option { value, "{label}" } }
            } }
            if matches!(kind.as_str(),"speech"|"command") { label { class:"form-field", span { "Текст" } textarea { value:text(&a["text"]), maxlength:"4000", oninput:move |e| argument(draft,s,n,"text",json!(e.value())) } } }
            if kind=="volume" { Field { label:"Громкость 0–100%", kind:"number", value:text(&a["percent"]), onchange:move |v| argument(draft,s,n,"percent",json!(v)) } }
            if kind=="delay" { Field { label:"Пауза, секунд (0–300)", kind:"number", value:text(&a["seconds"]), onchange:move |v| argument(draft,s,n,"seconds",json!(v)) } }
            if kind=="animation" { div { class:"automation-grid",
                Field { label:"ID анимации, объявленной колонкой", value:text(&a["animation"]), onchange:move |v| argument(draft,s,n,"animation",json!(v)) }
                Field { label:"Длительность, мс (100–60000)", kind:"number", value:text(&a["duration_ms"]), onchange:move |v| argument(draft,s,n,"duration_ms",json!(v)) }
            } }
            if kind=="light" { div { class:"automation-grid",
                label { class:"form-field", span { "Подсветка" } select { value:text(&a["target"]), onchange:move |e| argument(draft,s,n,"target",json!(e.value())), option { value:"ring", "Верхнее кольцо" } option { value:"logo", "Логотип" } } }
                label { class:"form-field", span { "Эффект" } select { value:text(&a["animation"]), onchange:move |e| argument(draft,s,n,"animation",json!(e.value())), option { value:"pulse", "Пульс" } option { value:"wave", "Волна" } option { value:"rainbow", "Радуга" } } }
                Field { label:"Цвет", kind:"color", value:text(&a["color"]), onchange:move |v| argument(draft,s,n,"color",json!(v)) }
                Field { label:"Яркость 0–100%", kind:"number", value:text(&a["brightness"]), onchange:move |v| argument(draft,s,n,"brightness",json!(v)) }
                Field { label:"Длительность, мс (1000–15000)", kind:"number", value:text(&a["duration_ms"]), onchange:move |v| argument(draft,s,n,"duration_ms",json!(v)) }
            } p { class:"field-help", "Использует отдельное ADB-подключение по сохранённому адресу колонки и проверяет framebuffer Миди." } }
            if kind=="scenario" { label { class:"form-field", span { "Сценарий" } select { value:text(&a["scenario_id"]), onchange:move |e| argument(draft,s,n,"scenario_id",json!(e.value())), option { value:"", "Выберите сценарий" } for target in items(&all["scenarios"]) { option { value:text(&target["id"]), {text(&target["name"])} } } } } }
            if kind=="if" { ConditionEditor { draft, s, n } }
            if kind!="stop" { div { class:"automation-grid",
                Next { label:if kind=="if" {"Если да"} else {"После шага"}, value:value["next"].clone(), nodes:nodes.clone(), onchange:move |v| node(draft,s,n,"next",v) }
                if kind=="if" { Next { label:"Если нет", value:value["otherwise"].clone(), nodes, onchange:move |v| node(draft,s,n,"otherwise",v) } }
            } }
        }
    }
}

#[component]
fn Next(label: String, value: Value, nodes: Vec<Value>, onchange: EventHandler<Value>) -> Element {
    let selected = if value.is_null() {
        "__next".into()
    } else {
        text(&value)
    };
    rsx! { label { class:"form-field", span { "{label}" } select { value:selected, onchange:move |e| onchange.call(if e.value()=="__next" {Value::Null} else {json!(e.value())}), option { value:"__next", "Следующий шаг по порядку" } option { value:"", "Завершить" } for (index,node) in nodes.iter().enumerate() { option { value:text(&node["id"]), {format!("Шаг {} · {}",index+1,text(&node["id"]))} } } } } }
}

#[component]
fn ConditionEditor(draft: Signal<Value>, s: usize, n: usize) -> Element {
    let value = draft.read()["scenarios"][s]["nodes"][n]["action"]["condition"].clone();
    let kind = text(&value["kind"]);
    rsx! {
        label { class:"form-field", span { "Проверка" } select { value:kind.clone(), onchange:move |e| { let kind=e.value(); let value=match kind.as_str() { "weekday"=>json!({"kind":kind,"days":[1,2,3,4,5]}), "time"=>json!({"kind":kind,"start":"08:00","end":"22:00"}), _=>json!({"kind":kind,"expected":true}) }; argument(draft,s,n,"condition",value); }, option { value:"connected", "Подключение колонки" } option { value:"dnd", "Не беспокоить" } option { value:"weekday", "День недели" } option { value:"time", "Интервал времени" } } }
        if matches!(kind.as_str(),"connected"|"dnd") { label { class:"form-field", span { "Ожидаемое состояние" } select { value:if value["expected"]==true {"true"}else{"false"}, onchange:{let value=value.clone(); move |e| { let mut value=value.clone(); value["expected"]=json!(e.value()=="true"); argument(draft,s,n,"condition",value); }}, option { value:"true", "Да / включено" } option { value:"false", "Нет / выключено" } } } }
        if kind=="weekday" { Days { value:items(&value["days"]), onchange:{let value=value.clone(); move |days| { let mut value=value.clone();value["days"]=days;argument(draft,s,n,"condition",value); }} } }
        if kind=="time" { div { class:"automation-grid",
            Field { label:"С", kind:"time", value:text(&value["start"]), onchange:{let value=value.clone();move |v| {let mut value=value.clone();value["start"]=json!(v);argument(draft,s,n,"condition",value);}} }
            Field { label:"До (не включая)", kind:"time", value:text(&value["end"]), onchange:move |v| {let mut value=value.clone();value["end"]=json!(v);argument(draft,s,n,"condition",value);} }
        } }
    }
}

#[component]
pub(super) fn AlarmEditor(draft: Signal<Value>, index: usize) -> Element {
    let value = draft.read()["alarms"][index].clone();
    let repeat = value["date"].is_null();
    rsx! {
        section { class:"card settings-card",
            div { class:"automation-node-header", h3 { {text(&value["name"])} } button { class:"text-button", onclick:move |_| {let mut draft=draft;draft.with_mut(|d|{d["alarms"].as_array_mut().unwrap().remove(index);});}, "Удалить" } }
            div { class:"automation-grid",
                Field { label:"Название", value:text(&value["name"]), onchange:move |v| alarm(draft,index,"name",json!(v)) }
                Field { label:"Время", kind:"time", value:text(&value["time"]), onchange:move |v| alarm(draft,index,"time",json!(v)) }
                Field { label:"Часовой пояс IANA", value:text(&value["timezone"]), onchange:move |v| alarm(draft,index,"timezone",json!(v)) }
            }
            label { class:"toggle-field", input { r#type:"checkbox", checked:value["enabled"]==true, onchange:move |e| alarm(draft,index,"enabled",json!(e.checked())) } span { "Включён" } }
            label { class:"toggle-field", input { r#type:"checkbox", checked:repeat, onchange:move |e| { alarm(draft,index,"date",if e.checked(){Value::Null}else{json!("")}); alarm(draft,index,"weekdays",if e.checked(){json!([1,2,3,4,5,6,7])}else{json!([])}); } } span { "Повторять по дням недели" } }
            if repeat { Days { value:items(&value["weekdays"]), onchange:move |v| alarm(draft,index,"weekdays",v) } } else { Field { label:"Дата однократного звонка", kind:"date", value:text(&value["date"]), onchange:move |v| alarm(draft,index,"date",json!(v)) } }
            Field { label:"Локальный WAV на колонке (пусто — встроенный сигнал)", value:text(&value["sound"]), onchange:move |v| alarm(draft,index,"sound",json!(v)) }
            div { class:"automation-grid",
                Field { label:"Громкость 0–100%", kind:"number", value:text(&value["volume"]), onchange:move |v| alarm(draft,index,"volume",json!(v)) }
                Field { label:"Нарастание, секунд (0–300)", kind:"number", value:text(&value["ramp_seconds"]), onchange:move |v| alarm(draft,index,"ramp_seconds",json!(v)) }
                Field { label:"Отложить на минут (1–60)", kind:"number", value:text(&value["snooze_minutes"]), onchange:move |v| alarm(draft,index,"snooze_minutes",json!(v)) }
                Field { label:"Остановить через минут (1–30)", kind:"number", value:text(&value["timeout_minutes"]), onchange:move |v| alarm(draft,index,"timeout_minutes",json!(v)) }
            }
            p { class:"field-help", "При возврате летнего времени будильник звонит один раз за календарный день. Пропущенное время не воспроизводится задним числом. После изменения сохраните и передайте расписание на колонку." }
        }
    }
}

pub(super) fn numbers(document: &mut Value) -> Result<(), String> {
    fn convert(value: &mut Value, key: &str) -> Result<(), String> {
        if let Some(raw) = value[key].as_str() {
            let number = raw
                .parse::<u64>()
                .map_err(|_| format!("Поле {key} должно содержать целое неотрицательное число"))?;
            value[key] = json!(number);
        }
        Ok(())
    }
    if let Some(scenarios) = document["scenarios"].as_array_mut() {
        for scenario in scenarios {
            if let Some(nodes) = scenario["nodes"].as_array_mut() {
                for node in nodes {
                    for key in ["percent", "seconds", "brightness", "duration_ms"] {
                        if !node["action"][key].is_null() {
                            convert(&mut node["action"], key)?;
                        }
                    }
                }
            }
        }
    }
    if let Some(alarms) = document["alarms"].as_array_mut() {
        for alarm in alarms {
            for key in [
                "volume",
                "ramp_seconds",
                "snooze_minutes",
                "timeout_minutes",
            ] {
                convert(alarm, key)?;
            }
        }
    }
    Ok(())
}
