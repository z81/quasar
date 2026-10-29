//! Raw station directives of the management API: the part of protobufjs
//! `TDirective.fromObject` (TS push-directive) that pushes use, and the
//! directives TS pushes itself. Field names are the .proto ones (`Name`,
//! `Type`, `IsLedSilent`, `Payload`), scalars get JS coercions, a Struct is in
//! protobufjs form (`{ "fields": [{ "key": "k", "value": { "stringValue": "v" } }] }`),
//! and the TypeErrors protobufjs throws become errors.

use serde_json::{Map, Number, Value};

use crate::proto::google::protobuf::{self as pb, r#struct::FieldsEntry, value::Kind};
use crate::proto::n_alice::n_alice_api::TDirective;
use crate::proto::n_alice::n_alice_api::t_directive::PayloadType;

/// TS pushUpdateConfigDirective: the station fetches get_sync_info again
pub fn push_update_config() -> TDirective {
    TDirective {
        name: "push_update_config".to_string(),
        r#type: "client_action".to_string(),
        ..Default::default()
    }
}

/// The fields the Spotify bridge sends; other fields are ignored
pub fn from_object(value: &Value) -> Result<TDirective, String> {
    let object = object(value, ".NAlice.NAliceApi.TDirective")?;
    let mut directive = TDirective::default();
    if let Some(value) = field(object, "Type") {
        directive.r#type = js_string(value);
    }
    if let Some(value) = field(object, "Name") {
        directive.name = js_string(value);
    }
    directive.is_led_silent = field(object, "IsLedSilent").is_some_and(truthy);
    if let Some(payload) = field(object, "Payload") {
        directive.payload_type = Some(PayloadType::Payload(struct_from_object(
            payload,
            ".NAlice.NAliceApi.TDirective.Payload",
        )?));
    }
    Ok(directive)
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
