use dioxus::prelude::*;
use gloo_net::http::Request;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::{ControlState, Delivery, Device, Entry};

pub async fn refresh(mut state: ControlState) {
    let result = async {
        let response = Request::get("/devices")
            .send()
            .await
            .map_err(|_| "Не удалось связаться с сервером".to_string())?;
        if !response.ok() {
            return Err(format!(
                "Сервер не вернул список колонок (HTTP {})",
                response.status()
            ));
        }
        response
            .json::<Vec<Device>>()
            .await
            .map_err(|_| "Сервер вернул непонятный список колонок".to_string())
    }
    .await;
    match result {
        Ok(mut devices) => {
            devices.sort_by(|a, b| a.label().cmp(b.label()).then(a.duid.cmp(&b.duid)));
            if !devices
                .iter()
                .any(|device| device.duid == *state.selected.read())
            {
                state.selected.set(
                    devices
                        .first()
                        .map(|device| device.duid.clone())
                        .unwrap_or_default(),
                );
            }
            state.devices.set(devices);
            state.server_error.set(None);
        }
        Err(error) => state.server_error.set(Some(error)),
    }
    state.loading.set(false);
}

pub fn submit(state: ControlState) {
    let text = state.text.read().trim().to_string();
    if text.is_empty() {
        return;
    }
    let mode = (state.mode)();
    dispatch(
        state,
        (state.selected)(),
        mode.label().to_string(),
        text.clone(),
        mode.path().to_string(),
        json!({"eventText": text}),
        true,
    );
}

pub fn quick_action(state: ControlState, name: &str, label: &str) {
    dispatch(
        state,
        (state.selected)(),
        "Управление".to_string(),
        label.to_string(),
        "push-directive".to_string(),
        json!({"Name": name, "Type": "client_action", "IsLedSilent": true, "Payload": {"fields": []}}),
        false,
    );
}

pub fn repeat(mut state: ControlState, entry: Entry) {
    state.selected.set(entry.duid.clone());
    dispatch(
        state,
        entry.duid,
        entry.label,
        entry.text,
        entry.path,
        entry.body,
        false,
    );
}

pub fn dispatch(
    mut state: ControlState,
    duid: String,
    label: String,
    text: String,
    path: String,
    body: Value,
    clear: bool,
) {
    if (state.sending)() {
        return;
    }
    if state.server_error.read().is_some() {
        state.notice.set(Some(
            "Сначала восстановите соединение с сервером.".to_string(),
        ));
        return;
    }
    let device = state
        .devices
        .read()
        .iter()
        .find(|device| device.duid == duid)
        .cloned();
    let Some(device) = device.filter(Device::can_receive) else {
        state.notice.set(Some(
            "Колонка сейчас не подключена. Обновите список и попробуйте ещё раз.".to_string(),
        ));
        return;
    };
    state.sending.set(true);
    state.notice.set(None);
    let submitted_draft = clear.then(|| (state.text)());
    let now = js_sys::Date::new_0();
    let id = js_sys::Date::now() as u64;
    state.history.write().insert(
        0,
        Entry {
            id,
            time: format!("{:02}:{:02}", now.get_hours(), now.get_minutes()),
            station: device.label().to_string(),
            duid: duid.clone(),
            label,
            text: text.clone(),
            path: path.clone(),
            body: body.clone(),
            delivery: Delivery::Sending,
            detail: "Отправляем…".to_string(),
        },
    );
    state.history.write().truncate(100);
    // Delivery belongs to the app, not the composer/editor that initiated it.
    // Switching panels must not cancel it and leave the shared state pending.
    dioxus::core::spawn_forever(async move {
        let result = send(&duid, &path, &body).await;
        let (delivery, detail) = match result {
            Ok(Some(count)) if count > 0 => {
                (Delivery::Sent, "Передано в соединение колонки".to_string())
            }
            Ok(Some(_)) => (
                Delivery::Failed,
                "Колонка отключилась. Команда не передана.".to_string(),
            ),
            Ok(None) => (
                Delivery::Accepted,
                "Сервер принял запрос, но не сообщает, передан ли он колонке.".to_string(),
            ),
            Err(error) => (Delivery::Failed, error),
        };
        if matches!(delivery, Delivery::Sent | Delivery::Accepted)
            && submitted_draft
                .as_ref()
                .is_some_and(|draft| *state.text.read() == *draft)
        {
            state.text.set(String::new());
        }
        if delivery == Delivery::Failed {
            state.notice.set(Some(detail.clone()));
        }
        if let Some(entry) = state
            .history
            .write()
            .iter_mut()
            .find(|entry| entry.id == id)
        {
            entry.delivery = delivery;
            entry.detail = detail;
        }
        state.sending.set(false);
    });
}

async fn send(duid: &str, path: &str, body: &Value) -> Result<Option<usize>, String> {
    #[derive(Deserialize)]
    struct Receipt {
        #[serde(default)]
        delivered: Option<usize>,
    }

    let duid: String = js_sys::encode_uri_component(duid).into();
    let url = format!("/devices/{duid}/{path}");
    let request = Request::post(&url)
        .json(body)
        .map_err(|_| "Не удалось подготовить команду".to_string())?;
    let response = request.send().await.map_err(|_| {
        "Соединение прервалось. Доставка неизвестна; проверьте колонку перед повтором.".to_string()
    })?;
    if !response.ok() {
        return Err(format!(
            "Ошибка запроса (HTTP {}): {}",
            response.status(),
            response.text().await.unwrap_or_default()
        ));
    }
    response
        .json::<Receipt>()
        .await
        .map(|receipt| receipt.delivered)
        .map_err(|_| {
            "Сервер принял запрос, но не подтвердил передачу колонке. Проверьте её перед повтором."
                .to_string()
        })
}
