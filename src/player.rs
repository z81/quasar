//! Management player state comes from station requests and structured state
//! entries supplied by the existing Spotify/AirPlay bridge. Unknown values
//! remain unknown; sending a directive does not acknowledge its execution.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::Utc;
use prost::Message;
use serde::Serialize;
use serde_json::{json, Value};

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::directives;
use crate::proto::n_alice::n_alice_api::{TDeviceState, TDirective, TServerMessage};
use crate::proto::n_alice::t_alice_request_proto::t_request::DeviceStateType;
use crate::proto::n_alice::TAliceRequestProto;
use crate::remote::RemoteServers;

#[derive(Clone, Debug, Default, Serialize)]
pub struct PlayerStatus {
    pub connected: bool,
    pub state: String,
    pub source: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub updated_at: Option<String>,
    pub capabilities: Capabilities,
    #[serde(skip)]
    stream_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Capabilities {
    pub pause: bool,
    pub resume: bool,
    pub next: bool,
    pub previous: bool,
    pub set_volume: bool,
}

#[derive(Clone, Default)]
pub struct PlayerRegistry {
    players: Arc<Mutex<HashMap<String, PlayerStatus>>>,
    refreshes: Arc<Mutex<HashMap<String, Instant>>>,
}

impl PlayerRegistry {
    /// A station sends its state with voice/text requests, including typed
    /// callbacks. Both protobuf representations used by firmware are accepted.
    pub fn observe(&self, duid: &str, request: &TAliceRequestProto) {
        let Some(state) = request
            .request
            .as_ref()
            .and_then(|request| request.device_state_type.as_ref())
        else {
            return;
        };
        let decoded;
        let state = match state {
            DeviceStateType::DeviceState(state) => state,
            DeviceStateType::DeviceStateRaw(raw) => {
                let Ok(state) = TDeviceState::decode(raw.as_slice()) else {
                    return;
                };
                decoded = state;
                &decoded
            }
        };
        self.observe_state(duid, state);
    }

    pub fn observe_state(&self, duid: &str, state: &TDeviceState) {
        let mut players = self.players.lock().unwrap_or_else(PoisonError::into_inner);
        let player = players
            .entry(duid.to_string())
            .or_insert_with(|| PlayerStatus {
                state: "unknown".to_string(),
                ..Default::default()
            });
        if let Some(level) = state
            .sound_level
            .or_else(|| state.music.as_ref().and_then(|music| music.sound_level))
        {
            let max = state
                .sound_max_level
                .filter(|level| *level > 0)
                .unwrap_or(10);
            player.volume = Some(((u64::from(level) * 100 / u64::from(max)).min(100)) as u8);
        }
        if let Some(muted) = state
            .sound_muted
            .or_else(|| state.music.as_ref().and_then(|music| music.sound_muted))
        {
            player.muted = Some(muted);
        }
        if let Some(audio) = &state.audio_player {
            if let Some(state) = audio.player_state {
                player.state = match state {
                    1 => "idle",
                    2 => "playing",
                    3 => "paused",
                    4 => "buffering",
                    5 => "stopped",
                    6 => "finished",
                    _ => "unknown",
                }
                .to_string();
            }
            if let Some(track) = &audio.currently_playing {
                player.stream_id = track.stream_id.clone();
                player.title = nonempty(track.title.as_deref());
                player.artist = nonempty(track.sub_title.as_deref());
                // Stream type names the protocol media type, not its origin.
                player.source = Some("Аудиопоток".to_string());
            } else if matches!(player.state.as_str(), "idle" | "stopped" | "finished") {
                player.title = None;
                player.artist = None;
                player.source = None;
                player.stream_id = None;
            }
        } else if let Some(music) = &state.music {
            if let Some(paused) = music.player.as_ref().and_then(|player| player.pause) {
                player.state = if paused { "paused" } else { "playing" }.to_string();
            }
            if let Some(track) = music
                .currently_playing
                .as_ref()
                .and_then(|track| track.raw_track_info.as_ref())
            {
                let track = directives::struct_to_json(track);
                player.title = nonempty(track["title"].as_str());
                player.artist = track["artists"]
                    .as_array()
                    .map(|artists| {
                        artists
                            .iter()
                            .filter_map(|artist| artist["name"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|artists| !artists.is_empty());
                player.source = Some("Яндекс Музыка".to_string());
            }
        }
        player.updated_at = Some(Utc::now().to_rfc3339());
    }

    fn snapshot(&self, duid: &str) -> PlayerStatus {
        self.players
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(duid)
            .cloned()
            .unwrap_or_else(|| PlayerStatus {
                state: "unknown".to_string(),
                ..Default::default()
            })
    }

    fn refresh_due(&self, duid: &str) -> bool {
        let mut refreshes = self
            .refreshes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if refreshes
            .get(duid)
            .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(5))
        {
            return false;
        }
        refreshes.insert(duid.to_string(), now);
        true
    }
}

/// Requests the station's current state without changing playback or speaking.
pub fn refresh_message() -> TServerMessage {
    use crate::proto::n_alice::n_alice_api::{
        t_server_message, t_sync_request, TEventHeader, TGetDeviceState, TSyncPush, TSyncRequest,
    };
    let id = uuid::Uuid::new_v4().to_string();
    TServerMessage {
        message: Some(t_server_message::Message::Event(t_server_message::TEvent {
            header: Some(TEventHeader {
                message_id: id.clone(),
                ..Default::default()
            }),
            event: Some(t_server_message::t_event::Event::SyncPush(TSyncPush {
                push_id: id,
                sync_request: Some(TSyncRequest {
                    request: Some(t_sync_request::Request::GetDeviceState(TGetDeviceState {
                        get_device_state_options: Some(Default::default()),
                        ..Default::default()
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            })),
        })),
        ..Default::default()
    }
}

/// Structured bridge state is tied to its configured station, so a bridge
/// cannot accidentally control a different device selected in the UI.
async fn bridge_state(remote: &RemoteServers, duid: &str) -> Option<Value> {
    let metadata = json!({"serverType":"quasar", "deviceId":duid});
    let entries = remote.state(duid, &metadata).await;
    let (_, value) = entries.get("player")?;
    let value: Value = serde_json::from_str(value).ok()?;
    (value["duid"].as_str() == Some(duid)).then_some(value)
}

pub async fn status(
    registry: &ConnectionRegistry,
    remote: &RemoteServers,
    duid: &str,
) -> PlayerStatus {
    if registry.is_connected(duid) && registry.player.refresh_due(duid) {
        registry.send(duid, StationCommand::RefreshPlayer);
    }
    let bridge = bridge_state(remote, duid).await;
    let mut player = registry.player.snapshot(duid);
    player.connected = registry.is_connected(duid);
    player.capabilities.pause = player.connected;
    player.capabilities.set_volume = player.connected;
    if let Some(bridge) = bridge {
        let different_stream = player
            .stream_id
            .as_deref()
            .zip(bridge["stream_id"].as_str())
            .is_some_and(|(native, bridged)| native != bridged);
        let native_other_source = matches!(player.state.as_str(), "playing" | "buffering")
            && bridge["state"].as_str() == Some("stopped")
            && (different_stream || player.source.as_deref() == Some("Яндекс Музыка"));
        if !native_other_source {
            if let Some(state) = bridge["state"]
                .as_str()
                .filter(|state| matches!(*state, "playing" | "paused" | "stopped"))
            {
                player.state = state.to_string();
            }
            player.source = nonempty(bridge["source"].as_str());
            player.title = nonempty(bridge["title"].as_str());
            player.artist = nonempty(bridge["artist"].as_str());
        }
        player.capabilities.resume =
            player.connected && !native_other_source && remote.owns("speaker_player_resume");
        let spotify = bridge["source"].as_str() == Some("Spotify");
        player.capabilities.next =
            player.connected && !native_other_source && spotify && remote.owns("spotify_next");
        player.capabilities.previous =
            player.connected && !native_other_source && spotify && remote.owns("spotify_previous");
        // The bridge is queried live, while the numeric volume is the last
        // value reported by the station and is deliberately not guessed.
    }
    player
}

pub struct PlayerError(pub axum::http::StatusCode, pub &'static str);

#[derive(Serialize)]
pub struct PlayerReceipt {
    pub delivered: usize,
    pub result: String,
}

/// Validated management actions. Station queueing and remote-provider results
/// remain distinct from an acknowledgement by physical playback hardware.
pub async fn action(
    registry: &ConnectionRegistry,
    remote: &RemoteServers,
    duid: &str,
    body: &Value,
) -> Result<PlayerReceipt, PlayerError> {
    use axum::http::StatusCode;
    if !registry.is_connected(duid) {
        return Err(PlayerError(StatusCode::CONFLICT, "Колонка не подключена"));
    }
    let name = body["action"].as_str().ok_or(PlayerError(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Не задано действие",
    ))?;
    let directive = match name {
        "set_volume" => {
            let volume = body["volume"]
                .as_u64()
                .filter(|volume| *volume <= 100)
                .ok_or(PlayerError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Громкость должна быть целым числом от 0 до 100",
                ))?;
            Some(client_action(
                "sound_set_level",
                json!({"new_level":volume.div_ceil(10),"new_percent_level":volume}),
            ))
        }
        "pause" | "stop" => Some(client_action("audio_stop", json!({}))),
        "resume" | "next" | "previous" => None,
        _ => {
            return Err(PlayerError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Неизвестное действие плеера",
            ))
        }
    };
    if let Some(directive) = directive {
        let delivered = registry.send(
            duid,
            StationCommand::Directives {
                directives: vec![directive],
            },
        );
        return Ok(PlayerReceipt {
            delivered,
            result: if delivered > 0 {
                "Директива отправлена колонке"
            } else {
                "Соединение с колонкой потеряно"
            }
            .to_string(),
        });
    }
    let current = status(registry, remote, duid).await;
    let (function, supported) = match name {
        "resume" => ("speaker_player_resume", current.capabilities.resume),
        "next" => ("spotify_next", current.capabilities.next),
        "previous" => ("spotify_previous", current.capabilities.previous),
        _ => unreachable!(),
    };
    if !supported {
        return Err(PlayerError(
            StatusCode::CONFLICT,
            "Источник не поддерживает это действие",
        ));
    }
    let result = remote
        .call_by_name(
            function,
            &Some(json!({})),
            duid,
            &json!({"serverType":"quasar", "deviceId":duid}),
        )
        .await
        .map_err(|_| PlayerError(StatusCode::BAD_GATEWAY, "Источник музыки недоступен"))?;
    if result.trim_start().starts_with("error:") {
        return Err(PlayerError(
            StatusCode::BAD_GATEWAY,
            "Источник не смог выполнить действие",
        ));
    }
    Ok(PlayerReceipt {
        delivered: 1,
        result,
    })
}

fn client_action(name: &str, payload: Value) -> TDirective {
    // Reuse the existing protobuf Struct conversion. The semantic-frame
    // envelope is replaced before this directive leaves the server.
    let mut directive = directives::mm_semantic_frame(&payload);
    directive.name = name.to_string();
    directive.r#type = "client_action".to_string();
    directive.is_led_silent = true;
    directive
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}
