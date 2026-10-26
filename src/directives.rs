//! Translates the LLM's station functions into real station directives,
//! mirroring routers/alice/base-provider.ts of the TypeScript version, plus
//! the server_action semantic frames of routers/alice/{directives,typed-payloads}.ts.

use serde_json::{Value, json};

use crate::agent::protocol::DirectiveResult;
use crate::proto::google::protobuf::{self as pb, r#struct::FieldsEntry, value::Kind};
use crate::proto::n_alice::n_alice_api::TDirective;
use crate::proto::n_alice::n_alice_api::t_directive::PayloadType;

/// The station directive for one LLM function call, or None when the call
/// is unknown or its arguments do not match (logged)
pub fn to_station_directive(call: &DirectiveResult) -> Option<TDirective> {
    let arguments = call.arguments.clone().unwrap_or(Value::Null);
    let directive = match call.name.as_str() {
        "smart_speaker_player_pause" => client_action("clear_queue", json!({}), true),
        "smart_speaker_player_rewind" => {
            let amount_ms = arguments["amountMs"].as_u64().filter(|amount| *amount > 0);
            let direction = arguments["direction"].as_str().filter(|direction| matches!(*direction, "Forward" | "Backward"));
            match (amount_ms, direction) {
                (Some(amount_ms), Some(direction)) => client_action(
                    "audio_player_rewind",
                    json!({ "amount_ms": amount_ms, "type": direction }),
                    true,
                ),
                _ => return invalid(call),
            }
        }
        "smart_speaker_bluetooth" => match arguments["enable"].as_bool() {
            Some(true) => client_action("start_bluetooth", json!({}), false),
            Some(false) => client_action("stop_bluetooth", json!({}), false),
            None => return invalid(call),
        },
        "smart_speaker_set_sound_volume" => match arguments["volume"].as_u64().filter(|volume| (1..=10).contains(volume)) {
            Some(volume) => client_action(
                "sound_set_level",
                json!({ "new_level": volume, "new_percent_level": volume * 10 }),
                true,
            ),
            None => return invalid(call),
        },
        "smart_speaker_set_sound_volume_louder" => client_action("sound_louder", json!({}), true),
        "smart_speaker_set_sound_volume_quieter" => client_action("sound_quiter", json!({}), true),
        "smart_speaker_play_iot_sound" => match arguments["isPowerOn"].as_bool() {
            Some(is_power_on) => client_action(
                "local_audio_file_play",
                json!({
                    "play_times": 1,
                    "sound_to_play": if is_power_on { "AliceIoTSoundsTurnOn" } else { "AliceIoTSoundsTurnOff" },
                }),
                true,
            ),
            None => return invalid(call),
        },
        _ => {
            tracing::warn!("no station directive for function '{}'", call.name);
            return None;
        }
    };
    Some(directive)
}

/// Tells the station to play the TTS stream of this response in the dialog channel
pub fn tts_play_placeholder() -> TDirective {
    TDirective {
        analytics_type: "tts_play_placeholder".to_string(),
        ..client_action("tts_play_placeholder", json!({ "channel": "Dialog" }), true)
    }
}

/// What a plain `text_input` command from the station maps to (TS `textToDirectiveMap`)
pub enum TextCommand {
    Directive(Box<TDirective>),
    /// "продолжи" resumes music through the music manager, which is not ported
    NeedsMusic,
    Unknown,
}

pub fn text_command(text: &str) -> TextCommand {
    let function = match text.trim().to_lowercase().as_str() {
        "громче" => "smart_speaker_set_sound_volume_louder",
        "тише" => "smart_speaker_set_sound_volume_quieter",
        // TS also saves the playback position for its music manager here
        "пауза" | "стоп" => "smart_speaker_player_pause",
        "продолжи" => return TextCommand::NeedsMusic,
        _ => return TextCommand::Unknown,
    };
    let call = DirectiveResult { name: function.to_string(), arguments: None };
    to_station_directive(&call).map_or(TextCommand::Unknown, |directive| TextCommand::Directive(Box::new(directive)))
}

/// A server_action the station sends back as a TextInput with the same
/// payload: the way to make the station start a request (TS `mmSemanticFrame`)
pub fn mm_semantic_frame(payload: &Value) -> TDirective {
    TDirective {
        name: "@@mm_semantic_frame".to_string(),
        r#type: "server_action".to_string(),
        payload_type: Some(PayloadType::Payload(to_struct(payload))),
        ..Default::default()
    }
}

/// An event for the dialog agent (TS `externalEventSemanticFrame`)
pub fn external_event_frame(event: &str) -> Value {
    json!({ "typed_semantic_frame": { "external_event_semantic_frame": { "event": event } } })
}

/// Text to speak as is (TS `ttsSemanticFrame`)
pub fn tts_frame(text: &str) -> Value {
    json!({ "typed_semantic_frame": { "tts_semantic_frame": { "text": text } } })
}

/// The second step of the TS "continue the session" handshake
pub fn continue_session_stage2_frame(dialog_id: Option<&str>) -> Value {
    json!({ "typed_semantic_frame": { "continue_session_stage2_semantic_frame": { "dialog_id": dialog_id } } })
}

/// A protobuf Struct (TextInput payloads) as JSON (TS `decodeProtobufStruct`)
pub fn struct_to_json(value: &pb::Struct) -> Value {
    Value::Object(
        value
            .fields
            .iter()
            .map(|entry| (entry.key.clone(), entry.value.as_ref().map_or(Value::Null, from_value)))
            .collect(),
    )
}

fn from_value(value: &pb::Value) -> Value {
    match &value.kind {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(number)) => serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number),
        Some(Kind::StringValue(text)) => Value::String(text.clone()),
        Some(Kind::BoolValue(flag)) => Value::Bool(*flag),
        Some(Kind::StructValue(inner)) => struct_to_json(inner),
        Some(Kind::ListValue(list)) => Value::Array(list.values.iter().map(from_value).collect()),
    }
}

fn client_action(name: &str, payload: Value, is_led_silent: bool) -> TDirective {
    TDirective {
        name: name.to_string(),
        r#type: "client_action".to_string(),
        is_led_silent,
        payload_type: Some(PayloadType::Payload(to_struct(&payload))),
        ..Default::default()
    }
}

fn invalid(call: &DirectiveResult) -> Option<TDirective> {
    tracing::warn!("invalid arguments for '{}': {:?}", call.name, call.arguments);
    None
}

fn to_struct(value: &Value) -> pb::Struct {
    pb::Struct {
        fields: value
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| FieldsEntry { key: key.clone(), value: Some(to_value(value)) })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn to_value(value: &Value) -> pb::Value {
    let kind = match value {
        Value::Null => Kind::NullValue(0),
        Value::Bool(flag) => Kind::BoolValue(*flag),
        Value::Number(number) => Kind::NumberValue(number.as_f64().unwrap_or_default()),
        Value::String(text) => Kind::StringValue(text.clone()),
        Value::Array(items) => Kind::ListValue(pb::ListValue { values: items.iter().map(to_value).collect() }),
        Value::Object(_) => Kind::StructValue(to_struct(value)),
    };
    pb::Value { kind: Some(kind) }
}
