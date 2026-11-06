//! The protobuf Struct callback representation uses the same discovery schema.
//! Only endpoint metadata and advertised capabilities are retained; keys and
//! commissioning codes are deliberately omitted.
use super::*;
use crate::proto::n_alice::{t_endpoint, TAnyCapabilityHolder};
use base64::{engine::general_purpose::STANDARD, Engine};

fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    let normalize = |name: &str| {
        name.bytes()
            .filter(|c| *c != b'_')
            .map(|c| c.to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    value
        .as_object()
        .and_then(|object| {
            object
                .iter()
                .find(|(key, _)| normalize(key) == normalize(name))
                .map(|(_, value)| value)
        })
        .unwrap_or(&Value::Null)
}
fn text(value: &Value, name: &str) -> String {
    field(value, name).as_str().unwrap_or_default().to_owned()
}
fn integer(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .and_then(|v| v.try_into().ok())
        .or_else(|| value.as_str()?.parse().ok())
}
fn bytes(value: &Value) -> Option<Vec<u8>> {
    STANDARD.decode(value.as_str()?).ok()
}
fn protocol(value: &Value) -> i32 {
    integer(value).unwrap_or_else(|| match value.as_str() {
        Some("PROTOCOL_ZIGBEE") => 1,
        Some("PROTOCOL_MATTER") => 2,
        _ => 0,
    })
}

impl Station {
    pub(super) fn callback_json(&mut self, value: &Value) {
        let structured = field(value, "TypedCallbackStruct");
        if !structured.is_null() {
            self.typed_json(
                &text(structured, "TypedCallbackName"),
                field(structured, "Payload"),
            );
        } else if let Some(bytes) = bytes(field(value, "TypedCallbackSerialized")) {
            if let Ok(any) = Any::decode(bytes.as_slice()) {
                self.typed(&any.type_url, &any.value);
            }
        }
    }

    pub(super) fn typed_json(&mut self, name: &str, value: &Value) {
        let endpoints = || {
            field(value, "DiscoveredEndpoints")
                .as_array()
                .into_iter()
                .flatten()
                .take(256)
                .filter_map(endpoint)
                .collect()
        };
        if name.ends_with("TFinishDiscoveryCallback") {
            let callback_id = field(value, "CallbackId")
                .as_u64()
                .and_then(|id| u32::try_from(id).ok());
            if let Some(callback_id) = callback_id {
                self.found(TFinishDiscoveryCallback {
                    callback_id,
                    session_id: text(value, "SessionId"),
                    system_info: text(value, "SystemInfo"),
                    discovered_endpoints: endpoints(),
                    ..Default::default()
                });
            }
        } else if name.ends_with("TFinishSystemDiscoveryCallback") {
            self.endpoints(endpoints());
        }
    }
}

fn endpoint(value: &Value) -> Option<TEndpoint> {
    let id = text(value, "Id");
    let meta = field(value, "Meta");
    let info = field(meta, "DeviceInfo");
    let protocol = protocol(field(info, "Protocol"));
    if id.is_empty() || ![1, 2].contains(&protocol) {
        return None;
    }
    let any_capabilities = field(value, "AnyCapabilities")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|holder| {
            let capability_type =
                integer(field(holder, "CapabilityType")).or_else(|| {
                    match field(holder, "CapabilityType").as_str() {
                        Some("OnOffCapabilityType") => Some(1),
                        Some("LevelCapabilityType") => Some(3),
                        _ => None,
                    }
                })?;
            Some(TAnyCapabilityHolder {
                capability_type,
                raw_capability: bytes(field(holder, "RawCapability"))?,
            })
        })
        .collect();
    let capabilities = field(value, "Capabilities")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|any| {
            let type_url = text(any, "TypeUrl");
            if !type_url.is_empty() {
                return Some(Any {
                    type_url,
                    value: bytes(field(any, "Value"))?,
                });
            }
            // Protobuf JSON expands Any as {"@type":..., ...message fields...}.
            let type_url = any.get("@type")?.as_str()?.to_owned();
            let raw = match type_url.rsplit('/').next()? {
                "NAlice.TOnOffCapability" => {
                    let state = field(any, "State");
                    TOnOffCapability {
                        meta: capability_meta(field(any, "Meta")),
                        state: (!state.is_null()).then(|| {
                            crate::proto::n_alice::t_on_off_capability::TState {
                                on: field(state, "On").as_bool().unwrap_or(false),
                                ..Default::default()
                            }
                        }),
                        ..Default::default()
                    }
                    .encode_to_vec()
                }
                "NAlice.TLevelCapability" => {
                    let parameters = field(any, "Parameters");
                    let instance = field(parameters, "Instance");
                    let instance = integer(instance).or_else(|| {
                        (instance.as_str() == Some("BrightnessInstance")).then_some(4)
                    })?;
                    let range = field(parameters, "Range");
                    let state = field(any, "State");
                    TLevelCapability {
                        meta: capability_meta(field(any, "Meta")),
                        parameters: Some(crate::proto::n_alice::t_level_capability::TParameters {
                            instance,
                            range: Some(crate::proto::n_alice::TRange {
                                min: field(range, "Min").as_f64()?,
                                max: field(range, "Max").as_f64()?,
                                precision: field(range, "Precision").as_f64().unwrap_or(1.0),
                            }),
                            ..Default::default()
                        }),
                        state: (!state.is_null()).then(|| {
                            crate::proto::n_alice::t_level_capability::TState {
                                level: field(state, "Level").as_f64().unwrap_or_default(),
                                ..Default::default()
                            }
                        }),
                    }
                    .encode_to_vec()
                }
                _ => return None,
            };
            Some(Any {
                type_url,
                value: raw,
            })
        })
        .collect();
    let state = field(value, "Status");
    let status =
        integer(field(state, "Status")).or_else(|| match field(state, "Status").as_str() {
            Some("Online") => Some(2),
            Some("Offline") => Some(1),
            _ => None,
        });
    Some(TEndpoint {
        id,
        meta: Some(t_endpoint::TMeta {
            device_info: Some(t_endpoint::TDeviceInfo {
                manufacturer: text(info, "Manufacturer"),
                model: text(info, "Model"),
                protocol,
                ..Default::default()
            }),
            ..Default::default()
        }),
        status: status.map(|status| t_endpoint::TStatus {
            status,
            ..Default::default()
        }),
        any_capabilities,
        capabilities,
    })
}

fn capability_meta(value: &Value) -> Option<crate::proto::n_alice::t_capability::TMeta> {
    if value.is_null() {
        return None;
    }
    let supported_directives = field(value, "SupportedDirectives")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            integer(v).or_else(|| EDirectiveType::from_str_name(v.as_str()?).map(|v| v as i32))
        })
        .collect();
    Some(crate::proto::n_alice::t_capability::TMeta {
        supported_directives,
        ..Default::default()
    })
}
