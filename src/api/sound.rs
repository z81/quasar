//! Midi sound configuration, with the pre-change settings retained for recovery.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::ApiState;
use crate::connections::StationCommand;
use crate::schedule_api::error;
use crate::stations::StationSummary;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SoundAction {
    Apply {
        enabled: bool,
        room_correction: bool,
        gains: [i8; 5],
    },
    Reset,
}

pub async fn status(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    match station(&state, &duid).await {
        Ok(station) => Json(snapshot(&state, &station)).into_response(),
        Err(response) => response,
    }
}

pub async fn update(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(input): Json<SoundAction>,
) -> Response {
    let station = match station(&state, &duid).await {
        Ok(station) => station,
        Err(response) => return response,
    };
    if station.platform != "cucumber" {
        return error(
            StatusCode::CONFLICT,
            "Эти настройки звука проверены только для Станции Миди",
        );
    }
    let mut config = station.quasar_config.clone();
    let previous = config["deviceConfig"].get("equalizer").cloned();
    match input {
        SoundAction::Apply {
            enabled,
            room_correction,
            gains,
        } => {
            if gains.iter().any(|gain| !(-12..=12).contains(gain)) {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Усиление каждой полосы: целое число от −12 до 12 дБ",
                );
            }
            let mut equalizer = previous
                .clone()
                .unwrap_or_else(crate::sound::default_equalizer);
            equalizer["enabled"] = json!(enabled);
            // Current Midi firmware reads this device-config key in its
            // EqualizerController, falling back to roomCorrectionDefault.
            equalizer["smartEnabled"] = json!(room_correction);
            equalizer["bands"] = json!(crate::sound::BANDS
                .iter()
                .zip(gains)
                .map(|((freq, width), gain)| json!({"freq":freq,"width":width,"gain":gain}))
                .collect::<Vec<_>>());
            config["deviceConfig"]["equalizer"] = equalizer;
        }
        // Send explicit Midi defaults. Removing a subscribed config section
        // does not establish that firmware will discard its previous state.
        SoundAction::Reset => {
            config["deviceConfig"]["equalizer"] = crate::sound::default_equalizer();
        }
    }
    // Keep one original backup and the immediately preceding value. Neither
    // contains account credentials or other station settings.
    let backup = json!({"equalizer":previous,"saved_at":chrono::Utc::now().to_rfc3339()});
    if let Err(cause) = state
        .storage
        .insert_control_value(&format!("sound-original:{duid}"), &backup)
        .await
    {
        return super::internal_error("sound backup", &cause);
    }
    if let Err(cause) = state
        .storage
        .set_control_value(&format!("sound-previous:{duid}"), &backup)
        .await
    {
        return super::internal_error("sound backup", &cause);
    }
    match state
        .storage
        .update_name_and_config(&duid, None, Some(&config), None)
        .await
    {
        Ok(Some(station)) => {
            let delivered = state.registry.send_one(
                &duid,
                StationCommand::Directives {
                    directives: vec![super::raw_directive::push_update_config()],
                },
            );
            Json(json!({"status":"saved","notified":delivered,"sound":snapshot(&state,&station)}))
                .into_response()
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "Колонка не найдена"),
        Err(cause) => super::internal_error("sound config update", &cause),
    }
}

fn snapshot(state: &ApiState, station: &StationSummary) -> Value {
    let equalizer = station.quasar_config["deviceConfig"].get("equalizer");
    let desired = equalizer
        .cloned()
        .unwrap_or_else(crate::sound::default_equalizer);
    let capability = state.registry.sound_capability(&station.duid);
    let supported = station.platform == "cucumber";
    json!({
        "supported":supported,"connected":state.registry.is_connected(&station.duid),
        "persistent":state.storage.is_persistent(),"configured":equalizer.is_some(),
        "reason":if supported { None } else { Some("Эти настройки звука проверены только для Станции Миди") },
        "enabled":desired["enabled"],"room_correction":desired.get("smartEnabled").cloned().unwrap_or(json!(true)),
        "gains":desired["bands"].as_array().map(|bands|bands.iter().map(|b|b["gain"].clone()).collect::<Vec<_>>()),
        "reported":capability.as_ref().and_then(|(_,capability)|capability.reported()),
        "equalizer_confirmed":capability.as_ref().and_then(|(_,capability)|capability.matches(&desired)),
        // EqualizerCapability has no Room Correction state field. A saved
        // value is deliberately not presented as firmware acknowledgement.
        "room_correction_confirmed":Value::Null,
    })
}

async fn station(state: &ApiState, duid: &str) -> Result<StationSummary, Response> {
    match state.storage.list().await {
        Ok(devices) => devices
            .into_iter()
            .find(|device| device.duid == duid)
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(cause) => Err(super::internal_error("sound device lookup", &cause)),
    }
}
