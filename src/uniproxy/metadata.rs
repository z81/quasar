//! Request metadata for remote function/state servers and the state prompt,
//! as TS builds it: the station's device state, the speaker biometry of
//! MatchedUser events and the server type.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use base64::Engine;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MapKey, Value as ReflectValue};
use serde_json::{Map, Value, json};

use crate::proto::n_alice::TAliceRequestProto;
use crate::proto::n_alice::t_alice_request_proto::t_request::DeviceStateType;

const DEVICE_STATE_MESSAGE: &str = "NAlice.NAliceApi.TDeviceState";

/// The speaker as classified by the station (TS handleMatchedUserEvent)
#[derive(Clone, Copy, Debug)]
pub struct Biometry {
    pub age: &'static str,
    pub gender: &'static str,
}

/// The classification a MatchedUser event carries, if any
pub fn biometry(matched_user: &TAliceRequestProto) -> Option<Biometry> {
    let classification = matched_user.request.as_ref()?.event.as_ref()?.biometry_classification.as_ref()?;
    if classification.simple.is_empty() {
        return None;
    }
    let class_of = |tag: &str| {
        classification
            .simple
            .iter()
            .find(|item| item.tag.as_deref() == Some(tag))
            .and_then(|item| item.class_name.as_deref())
    };
    Some(Biometry {
        age: match class_of("children") {
            Some("adult") => "adult",
            Some("child") => "child",
            _ => "unknown",
        },
        gender: match class_of("gender") {
            Some("female") => "female",
            Some("male") => "male",
            _ => "unknown",
        },
    })
}

/// Biometry of one voice input: MatchedUser may arrive while the utterance is
/// still recorded or already being transcribed, so the turn reads it only
/// once the text is known (as TS reads its voice metadata)
#[derive(Clone, Default)]
pub struct BiometrySlot(Arc<Mutex<Option<Biometry>>>);

impl BiometrySlot {
    pub fn set(&self, biometry: Biometry) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(biometry);
    }

    pub fn get(&self) -> Option<Biometry> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The station's TDeviceState of a request (stations send it serialized in DeviceStateRaw)
pub fn take_device_state(request: &mut TAliceRequestProto) -> Option<Vec<u8>> {
    match request.request.as_mut()?.device_state_type.take()? {
        DeviceStateType::DeviceStateRaw(raw) => Some(raw),
        DeviceStateType::DeviceState(state) => Some(state.encode_to_vec()),
    }
}

/// What TS passes along with a request as `metadata`
pub struct InputMetadata {
    pub device_state: Option<Value>,
    pub biometry: Option<Biometry>,
}

impl InputMetadata {
    pub fn new(device_state: Option<&[u8]>, biometry: Option<Biometry>) -> Self {
        Self { device_state: device_state.and_then(device_state_json), biometry }
    }

    /// The JSON remote servers receive: `deviceState`, `age`/`gender` and
    /// `serverType` (TS PROCESSOR_METADATA_SERVER_TYPE_KEY)
    pub fn to_json(&self) -> Value {
        let mut metadata = Map::new();
        if let Some(device_state) = &self.device_state {
            metadata.insert("deviceState".to_string(), device_state.clone());
        }
        if let Some(biometry) = self.biometry {
            metadata.insert("age".to_string(), json!(biometry.age));
            metadata.insert("gender".to_string(), json!(biometry.gender));
        }
        metadata.insert("serverType".to_string(), json!("quasar"));
        Value::Object(metadata)
    }
}

fn descriptors() -> Option<&'static DescriptorPool> {
    static POOL: OnceLock<Option<DescriptorPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        // written by build.rs from the same schemas as the generated types
        let encoded: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/alice_descriptors.bin"));
        DescriptorPool::decode(encoded)
            .inspect_err(|error| tracing::error!("protobuf descriptors are unusable: {error}"))
            .ok()
    })
    .as_ref()
}

/// The device state in the JSON shape of TS (protobufjs toJSON): JSON field
/// names, 64-bit integers and enums as strings, bytes as base64
fn device_state_json(raw: &[u8]) -> Option<Value> {
    let descriptor = descriptors()?.get_message_by_name(DEVICE_STATE_MESSAGE)?;
    match DynamicMessage::decode(descriptor, raw) {
        Ok(message) => Some(message_json(&message)),
        Err(error) => {
            tracing::warn!("undecodable device state: {error}");
            None
        }
    }
}

fn message_json(message: &DynamicMessage) -> Value {
    Value::Object(
        message
            .fields()
            .map(|(field, value)| (field.json_name().to_string(), value_json(value, &field.kind())))
            .collect(),
    )
}

fn value_json(value: &ReflectValue, kind: &Kind) -> Value {
    match value {
        ReflectValue::Bool(flag) => Value::Bool(*flag),
        ReflectValue::I32(number) => json!(number),
        ReflectValue::U32(number) => json!(number),
        ReflectValue::I64(number) => Value::String(number.to_string()),
        ReflectValue::U64(number) => Value::String(number.to_string()),
        ReflectValue::F32(number) => float_json(f64::from(*number)),
        ReflectValue::F64(number) => float_json(*number),
        ReflectValue::String(text) => Value::String(text.clone()),
        ReflectValue::Bytes(bytes) => Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        ReflectValue::EnumNumber(number) => match kind {
            Kind::Enum(descriptor) => {
                descriptor.get_value(*number).map_or_else(|| json!(number), |value| json!(value.name()))
            }
            _ => json!(number),
        },
        ReflectValue::Message(message) => message_json(message),
        ReflectValue::List(items) => Value::Array(items.iter().map(|item| value_json(item, kind)).collect()),
        ReflectValue::Map(entries) => {
            let value_kind = match kind {
                Kind::Message(entry) if entry.is_map_entry() => entry.map_entry_value_field().kind(),
                other => other.clone(),
            };
            Value::Object(entries.iter().map(|(key, value)| (map_key(key), value_json(value, &value_kind))).collect())
        }
    }
}

/// NaN and infinities are not JSON numbers; protobufjs writes them as strings
fn float_json(number: f64) -> Value {
    serde_json::Number::from_f64(number).map_or_else(|| Value::String(number.to_string()), Value::Number)
}

fn map_key(key: &MapKey) -> String {
    match key {
        MapKey::Bool(flag) => flag.to_string(),
        MapKey::I32(number) => number.to_string(),
        MapKey::I64(number) => number.to_string(),
        MapKey::U32(number) => number.to_string(),
        MapKey::U64(number) => number.to_string(),
        MapKey::String(text) => text.clone(),
    }
}
