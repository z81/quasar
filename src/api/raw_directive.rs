//! Raw station directives of the management API: protobufjs
//! `TDirective.fromObject` (TS push-directive) and the directives TS pushes
//! itself. Field names are the .proto ones (`Name`, `Type`, `IsLedSilent`...),
//! scalars get JS coercions, a Struct is in protobufjs form
//! (`{ "fields": [{ "key": "k", "value": { "stringValue": "v" } }] }`), and the
//! TypeErrors protobufjs throws become errors.

use base64::Engine;
use serde_json::{Map, Number, Value};

use crate::proto::google::protobuf::{self as pb, r#struct::FieldsEntry, value::Kind};
use crate::proto::n_alice::n_alice_api::t_directive::{
    OptionalMultiroomSessionId, OptionalPersId, PayloadType, TOnFinishEvent, t_on_finish_event,
};
use crate::proto::n_alice::n_alice_api::{
    TDirective, TLocalIoTSource, TLocalScenarioSource, TSourceInfo, t_source_info,
};
use crate::proto::n_alice::t_capability::EDirectiveType;

/// TS pushUpdateConfigDirective: the station fetches get_sync_info again
pub fn push_update_config() -> TDirective {
    TDirective {
        name: "push_update_config".to_string(),
        r#type: "client_action".to_string(),
        ..Default::default()
    }
}

pub fn from_object(value: &Value) -> Result<TDirective, String> {
    let object = object(value, ".NAlice.NAliceApi.TDirective")?;
    let mut directive = TDirective::default();
    if let Some(value) = field(object, "Type") {
        directive.r#type = js_string(value);
    }
    if let Some(value) = field(object, "Name") {
        directive.name = js_string(value);
    }
    if let Some(value) = field(object, "AnalyticsType") {
        directive.analytics_type = js_string(value);
    }
    directive.ignore_answer = field(object, "IgnoreAnswer").is_some_and(truthy);
    if let Some(ids) = object.get("RoomDeviceIds").filter(|ids| truthy(ids)) {
        let Value::Array(ids) = ids else {
            return Err(".NAlice.NAliceApi.TDirective.RoomDeviceIds: array expected".to_string());
        };
        directive.room_device_ids = ids.iter().map(js_string).collect();
    }
    directive.is_led_silent = field(object, "IsLedSilent").is_some_and(truthy);
    if let Some(endpoint_id) = field(object, "EndpointId") {
        let endpoint_id = self::object(endpoint_id, ".NAlice.NAliceApi.TDirective.EndpointId")?;
        directive.endpoint_id = Some(pb::StringValue {
            value: field(endpoint_id, "value").map(js_string).unwrap_or_default(),
        });
    }
    directive.is_parallel = field(object, "IsParallel").is_some_and(truthy);
    if let Some(value) = field(object, "TimeBeforeDropMs") {
        directive.time_before_drop_ms = to_uint32(to_number(value));
    }
    if let Some(on_finish) = field(object, "OnFinish") {
        let on_finish = self::object(on_finish, ".NAlice.NAliceApi.TDirective.OnFinish")?;
        let event = field(on_finish, "TypedCallbackRequest")
            .map(|request| {
                struct_from_object(request, ".NAlice.NAliceApi.TDirective.TOnFinishEvent.TypedCallbackRequest")
            })
            .transpose()?
            .map(t_on_finish_event::Event::TypedCallbackRequest);
        directive.on_finish = Some(TOnFinishEvent { event });
    }
    if let Some(source_info) = field(object, "SourceInfo") {
        directive.source_info = Some(source_info_from_object(source_info)?);
    }
    directive.attach_to_multiroom = field(object, "AttachToMultiroom").is_some_and(truthy);
    match object.get("DirectiveType") {
        Some(Value::Number(number)) => directive.directive_type = number.as_f64().map_or(0, |value| value as i32),
        Some(Value::String(name)) => {
            if let Some(directive_type) = EDirectiveType::from_str_name(name) {
                directive.directive_type = directive_type as i32;
            }
        }
        _ => {}
    }
    if let Some(payload) = field(object, "Payload") {
        directive.payload_type = Some(PayloadType::Payload(struct_from_object(
            payload,
            ".NAlice.NAliceApi.TDirective.Payload",
        )?));
    }
    // both set: protobufjs encodes both and the decoder keeps the later field
    match field(object, "PayloadRaw") {
        Some(Value::String(encoded)) => {
            let engine = base64::engine::GeneralPurpose::new(
                &base64::alphabet::STANDARD,
                base64::engine::GeneralPurposeConfig::new()
                    .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
            );
            let bytes = engine.decode(encoded).map_err(|_| "invalid encoding".to_string())?;
            directive.payload_type = Some(PayloadType::PayloadRaw(bytes));
        }
        Some(Value::Array(bytes)) => {
            let bytes = bytes.iter().map(|byte| to_uint32(to_number(byte)).to_le_bytes()[0]).collect();
            directive.payload_type = Some(PayloadType::PayloadRaw(bytes));
        }
        _ => {}
    }
    if let Some(value) = field(object, "MultiroomSessionId") {
        directive.optional_multiroom_session_id =
            Some(OptionalMultiroomSessionId::MultiroomSessionId(js_string(value)));
    }
    if let Some(value) = field(object, "PersId") {
        directive.optional_pers_id = Some(OptionalPersId::PersId(js_string(value)));
    }
    directive.id = field(object, "Id").map(js_string);
    Ok(directive)
}

fn source_info_from_object(value: &Value) -> Result<TSourceInfo, String> {
    let object = object(value, ".NAlice.NAliceApi.TDirective.SourceInfo")?;
    let mut source_info = TSourceInfo::default();
    if let Some(source) = field(object, "LocalIoTSource") {
        let source = self::object(source, ".NAlice.NAliceApi.TSourceInfo.LocalIoTSource")?;
        source_info.source = Some(t_source_info::Source::LocalIoTSource(TLocalIoTSource {
            endpoint_id: field(source, "EndpointId").map(js_string).unwrap_or_default(),
        }));
    }
    if let Some(source) = field(object, "LocalScenarioSource") {
        let source = self::object(source, ".NAlice.NAliceApi.TSourceInfo.LocalScenarioSource")?;
        source_info.source = Some(t_source_info::Source::LocalScenarioSource(TLocalScenarioSource {
            scenario_id: field(source, "ScenarioId").map(js_string).unwrap_or_default(),
        }));
    }
    Ok(source_info)
}

fn struct_from_object(value: &Value, name: &str) -> Result<pb::Struct, String> {
    let object = object(value, name)?;
    let mut result = pb::Struct::default();
    if let Some(fields) = object.get("fields").filter(|fields| truthy(fields)) {
        let Value::Array(fields) = fields else {
            return Err(".google.protobuf.Struct.fields: array expected".to_string());
        };
        for entry in fields {
            let entry = self::object(entry, ".google.protobuf.Struct.fields")?;
            result.fields.push(FieldsEntry {
                key: field(entry, "key").map(js_string).unwrap_or_default(),
                value: field(entry, "value")
                    .map(|value| value_from_object(value, ".google.protobuf.Struct.FieldsEntry.value"))
                    .transpose()?,
            });
        }
    }
    Ok(result)
}

/// Every kind present is set in field order, the last one wins (as on the wire)
fn value_from_object(value: &Value, name: &str) -> Result<pb::Value, String> {
    let object = object(value, name)?;
    let mut kind = match object.get("nullValue") {
        Some(Value::String(null)) if null == "NULL_VALUE" => Some(Kind::NullValue(0)),
        Some(Value::Number(null)) => Some(Kind::NullValue(null.as_f64().map_or(0, |null| null as i32))),
        _ => None,
    };
    if let Some(number) = field(object, "numberValue") {
        kind = Some(Kind::NumberValue(to_number(number)));
    }
    if let Some(text) = field(object, "stringValue") {
        kind = Some(Kind::StringValue(js_string(text)));
    }
    if let Some(flag) = field(object, "boolValue") {
        kind = Some(Kind::BoolValue(truthy(flag)));
    }
    if let Some(inner) = field(object, "structValue") {
        kind = Some(Kind::StructValue(struct_from_object(inner, ".google.protobuf.Value.structValue")?));
    }
    if let Some(list) = field(object, "listValue") {
        let list = self::object(list, ".google.protobuf.Value.listValue")?;
        let mut values = Vec::new();
        if let Some(items) = list.get("values").filter(|items| truthy(items)) {
            let Value::Array(items) = items else {
                return Err(".google.protobuf.ListValue.values: array expected".to_string());
            };
            for item in items {
                values.push(value_from_object(item, ".google.protobuf.ListValue.values")?);
            }
        }
        kind = Some(Kind::ListValue(pb::ListValue { values }));
    }
    Ok(pb::Value { kind })
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, String> {
    value.as_object().ok_or_else(|| format!("{name}: object expected"))
}

/// A set field: protobufjs skips null and undefined
fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).filter(|value| !value.is_null())
}

/// JS `String(value)`
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => js_number_string(number),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_string(item) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn js_number_string(number: &Number) -> String {
    match number.as_f64() {
        Some(float) if !number.is_i64() && !number.is_u64() && float.fract() == 0.0 && float.abs() < 1e21 => {
            format!("{float:.0}")
        }
        _ => number.to_string(),
    }
}

/// JS truthiness
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JS `Number(value)`
fn to_number(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::String(text) if text.trim().is_empty() => 0.0,
        Value::String(text) => text.trim().parse().unwrap_or(f64::NAN),
        Value::Null => 0.0,
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

/// JS `value >>> 0`
fn to_uint32(value: f64) -> u32 {
    if value.is_finite() {
        value.trunc().rem_euclid(4_294_967_296.0) as u32
    } else {
        0
    }
}
