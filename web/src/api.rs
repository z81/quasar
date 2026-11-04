use dioxus::prelude::*;
use gloo_net::http::{Request, Response};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::{AuthStatus, ControlState, Device, Entry, Favorite, Player};

pub fn encoded(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}

pub async fn checked(mut state: ControlState, response: Response) -> Result<Response, String> {
    if response.status() == 401 {
        // Protection may have been enabled since the last /auth/status poll.
        // Close the gate immediately; repeated 401s must not retrigger effects.
        let mut auth = *state.auth.peek();
        auth.configured = true;
        auth.authenticated = false;
        if *state.auth.peek() != auth {
            state.auth.set(auth);
        }
        return Err("Войдите, чтобы продолжить работу.".to_string());
    }
    if response.ok() {
        return Ok(response);
    }
    let status = response.status();
    let message = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&message)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_owned))
        .unwrap_or(message);
    Err(format!(
        "HTTP {status}: {}",
        detail.chars().take(500).collect::<String>()
    ))
}

pub async fn get<T: DeserializeOwned>(state: ControlState, url: &str) -> Result<T, String> {
    let response = Request::get(url)
        .send()
        .await
        .map_err(|_| "Не удалось связаться с сервером".to_string())?;
    checked(state, response)
        .await?
        .json::<T>()
        .await
        .map_err(|_| "Сервер вернул непонятный ответ".to_string())
}

pub async fn mutate(
    state: ControlState,
    method: &str,
    url: &str,
    body: Option<&Value>,
) -> Result<Response, String> {
    let request = match method {
        "PATCH" => Request::patch(url),
        "PUT" => Request::put(url),
        "DELETE" => Request::delete(url),
        _ => Request::post(url),
    };
    let request = if let Some(body) = body {
        request
            .json(body)
            .map_err(|_| "Не удалось подготовить запрос".to_string())?
    } else {
        request
            .build()
            .map_err(|_| "Не удалось подготовить запрос".to_string())?
    };
    let response = request.send().await.map_err(|_| {
        "Соединение прервалось. Результат запроса неизвестен; проверьте историю перед повтором."
            .to_string()
    })?;
    checked(state, response).await
}

pub async fn refresh_auth(mut state: ControlState) {
    match get::<AuthStatus>(state, "/auth/status").await {
        Ok(auth) => {
            if *state.auth.peek() != auth {
                state.auth.set(auth);
            }
            state.auth_error.set(None);
            if !*state.auth_loaded.peek() {
                state.auth_loaded.set(true);
            }
        }
        Err(error) => state.auth_error.set(Some(error)),
    }
}

pub async fn refresh(mut state: ControlState) {
    match get::<Vec<Device>>(state, "/devices").await {
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
                state.player.set(None);
            }
            state.devices.set(devices);
            state.server_error.set(None);
        }
        Err(error) => state.server_error.set(Some(error)),
    }
    state.loading.set(false);
}

pub async fn refresh_history(mut state: ControlState) {
    match get::<Vec<Entry>>(state, "/control/history").await {
        Ok(history) => {
            state.history.set(history);
            state.history_error.set(None);
        }
        Err(error) => state.history_error.set(Some(error)),
    }
}

pub async fn refresh_saved(mut state: ControlState) {
    match get::<Vec<Favorite>>(state, "/control/favorites").await {
        Ok(favorites) => state.favorites.set(favorites),
        Err(error) => state
            .notice
            .set(Some(format!("Не удалось загрузить избранное: {error}"))),
    }
    #[derive(Deserialize)]
    struct Status {
        persistent: bool,
    }
    if let Ok(status) = get::<Status>(state, "/control/status").await {
        state.persistent.set(Some(status.persistent));
    }
}

pub async fn refresh_player(mut state: ControlState) {
    let duid = (state.selected)();
    if duid.is_empty() {
        state.player.set(None);
        return;
    }
    let result = get::<Player>(state, &format!("/devices/{}/player", encoded(&duid))).await;
    if duid != *state.selected.read() {
        return;
    }
    match result {
        Ok(player) => {
            state.player.set(Some(player));
            state.player_error.set(None);
        }
        Err(error) => {
            state.player.set(None);
            state.player_error.set(Some(error));
        }
    }
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
        mode.path().to_string(),
        json!({"eventText": text}),
        true,
    );
}

pub fn quick_action(state: ControlState, name: &str, _label: &str) {
    dispatch(
        state,
        (state.selected)(),
        "push-directive".to_string(),
        json!({"Name": name, "Type": "client_action", "IsLedSilent": true, "Payload": {"fields": []}}),
        false,
    );
}

pub fn repeat(mut state: ControlState, entry: Entry) {
    state.selected.set(entry.duid.clone());
    state.player.set(None);
    dispatch(state, entry.duid, entry.path, entry.body, false);
}

pub fn dispatch(mut state: ControlState, duid: String, path: String, body: Value, clear: bool) {
    if (state.sending)() {
        return;
    }
    if state.server_error.read().is_some() {
        state.notice.set(Some(
            "Сначала восстановите соединение с сервером.".to_string(),
        ));
        return;
    }
    if !state
        .devices
        .read()
        .iter()
        .any(|device| device.duid == duid && device.can_receive())
    {
        state.notice.set(Some(
            "Колонка сейчас не подключена. Обновите список и попробуйте ещё раз.".to_string(),
        ));
        return;
    }
    state.sending.set(true);
    state.notice.set(None);
    let submitted_draft = clear.then(|| (state.text)());
    let url = if path.starts_with("/devices/") {
        path
    } else {
        format!("/devices/{}/{path}", encoded(&duid))
    };
    dioxus::core::spawn_forever(async move {
        #[derive(Deserialize)]
        struct Receipt {
            delivered: usize,
        }
        match mutate(state, "POST", &url, Some(&body)).await {
            Ok(response) => match response.json::<Receipt>().await {
                Ok(receipt) if receipt.delivered > 0 => {
                    if submitted_draft.as_ref().is_some_and(|draft| *state.text.read() == *draft) { state.text.set(String::new()); }
                }
                Ok(_) => state.notice.set(Some("Колонка отключилась. Команда не передана.".to_string())),
                Err(_) => state.notice.set(Some("Сервер принял запрос, но результат неизвестен. Проверьте историю перед повтором.".to_string())),
            },
            Err(error) => state.notice.set(Some(error)),
        }
        state.sending.set(false);
        refresh_history(state).await;
        refresh_player(state).await;
    });
}

pub fn player_action(state: ControlState, action: &str, volume: Option<u8>) {
    let body = if let Some(volume) = volume {
        json!({"action":action,"volume":volume})
    } else {
        json!({"action":action})
    };
    dispatch(state, (state.selected)(), "player".to_string(), body, false);
}

pub async fn create_favorite(
    mut state: ControlState,
    label: String,
    duid: String,
    path: String,
    body: Value,
) -> Result<(), String> {
    let path = if path.starts_with("/devices/") {
        path
    } else {
        format!("/devices/{}/{path}", encoded(&duid))
    };
    let body = json!({"label":label.trim(),"duid":duid,"path":path,"body":body});
    mutate(state, "POST", "/control/favorites", Some(&body)).await?;
    match get::<Vec<Favorite>>(state, "/control/favorites").await {
        Ok(values) => state.favorites.set(values),
        Err(error) => state.notice.set(Some(format!(
            "Команда сохранена, но не удалось обновить список: {error}"
        ))),
    }
    Ok(())
}

pub fn save_favorite(
    mut state: ControlState,
    label: String,
    duid: String,
    path: String,
    body: Value,
) {
    if label.trim().is_empty() {
        return;
    }
    dioxus::core::spawn_forever(async move {
        if let Err(error) = create_favorite(state, label, duid, path, body).await {
            state.notice.set(Some(error));
        }
    });
}
