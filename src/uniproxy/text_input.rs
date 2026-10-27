//! What a TextInput asks for (TS handleTextInputEvent): mostly server_action
//! payloads the server pushed before (external events, TTS, typed requests,
//! the continue handshake) or the station's play buttons, plus plain text commands.

use prost::Message as _;
use serde_json::Value;

use crate::directives::struct_to_json;
use crate::proto::n_alice::n_alice_api::TSemanticFrameRequestData;
use crate::proto::n_alice::n_alice_api::t_typed_semantic_frame::Type as TypedFrame;
use crate::proto::n_alice::t_event::PayloadType;
use crate::proto::n_alice::{EEventType, TAliceRequestProto};

pub enum TextInputKind {
    /// An event pushed through the API: answered by the dialog agent
    ExternalEvent(String),
    /// Text pushed through the API: spoken as is
    Tts(String),
    /// Text pushed through the API as if the user said it: answered by the
    /// dialog agent like a voice input
    Utterance(String),
    /// Step 1 of the TS keep-alive handshake: answered with a push of step 2
    ContinueStage1 {
        dialog_id: Option<String>,
    },
    /// Step 2 of the TS keep-alive handshake: fetches the rest of an answer
    ContinueStage2 {
        dialog_id: Option<String>,
    },
    /// A text command (TS `textToDirectiveMap`)
    Text(String),
    Unknown(String),
}

pub fn classify(request: &TAliceRequestProto) -> TextInputKind {
    let Some(event) = request.request.as_ref().and_then(|request| request.event.as_ref()) else {
        return TextInputKind::Unknown("no event in the request".to_string());
    };
    let is_server_action = event.r#type == Some(EEventType::ServerAction as i32);
    match &event.payload_type {
        Some(PayloadType::Payload(payload)) if is_server_action => classify_payload(&struct_to_json(payload)),
        Some(PayloadType::PayloadRaw(raw))
            if is_server_action && event.name.as_deref() == Some("@@mm_semantic_frame") =>
        {
            classify_semantic_frame(raw)
        }
        _ if event.r#type == Some(EEventType::TextInput as i32) => {
            TextInputKind::Text(event.text.clone().unwrap_or_default())
        }
        _ => TextInputKind::Unknown(format!("event type {:?}, name {:?}", event.r#type, event.name)),
    }
}

fn classify_payload(payload: &Value) -> TextInputKind {
    let frame = |name: &str| payload.pointer(&format!("/typed_semantic_frame/{name}")).is_some_and(truthy);
    let text = |pointer: &str| {
        payload.pointer(pointer).and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_string)
    };

    if frame("music_play_semantic_frame") {
        play_button()
    } else if let Some(event) = text("/typed_semantic_frame/external_event_semantic_frame/event") {
        TextInputKind::ExternalEvent(event)
    } else if let Some(text) = text("/typed_semantic_frame/tts_semantic_frame/text") {
        TextInputKind::Tts(text)
    } else if let Some(text) = text("/typed_semantic_frame/user_utterance_semantic_frame/text") {
        TextInputKind::Utterance(text)
    } else if frame("continue_session_stage1_semantic_frame") {
        TextInputKind::ContinueStage1 {
            dialog_id: text("/typed_semantic_frame/continue_session_stage1_semantic_frame/dialog_id"),
        }
    } else if frame("continue_session_stage2_semantic_frame") {
        TextInputKind::ContinueStage2 {
            dialog_id: text("/typed_semantic_frame/continue_session_stage2_semantic_frame/dialog_id"),
        }
    } else {
        TextInputKind::Unknown(format!("server_action {payload}"))
    }
}

/// The station's play and continue buttons: a request to the assistant, which
/// resumes the music if a music service gave it the functions for that
fn play_button() -> TextInputKind {
    TextInputKind::Utterance("продолжи музыку".to_string())
}

/// `@@mm_semantic_frame` with a serialized TSemanticFrameRequestData: the
/// station's play/continue buttons
fn classify_semantic_frame(raw: &[u8]) -> TextInputKind {
    match TSemanticFrameRequestData::decode(raw) {
        Ok(request) => match request.typed_semantic_frame.and_then(|frame| frame.r#type) {
            Some(TypedFrame::MusicPlaySemanticFrame(_) | TypedFrame::PlayerContinueSemanticFrame(_)) => play_button(),
            other => TextInputKind::Unknown(format!("semantic frame {other:?}")),
        },
        Err(error) => TextInputKind::Unknown(format!("undecodable semantic frame: {error}")),
    }
}

/// JavaScript truthiness, with which TS tests the payload paths
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
