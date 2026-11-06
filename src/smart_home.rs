//! Native Zigbee/Matter commissioning over the station's Alice API connection.
//! Network backups and commissioning credentials never enter API responses.

mod events;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::proto::google::protobuf::StringValue;
use crate::proto::n_alice::n_alice_api::{t_directive::PayloadType, TDirective};
use crate::proto::n_alice::t_capability::{ECapabilityType, EDirectiveType};
use crate::proto::n_alice::t_iot_discovery_capability as discovery;
use crate::proto::n_alice::{
    TEndpoint, TIotDiscoveryCapability, TLevelCapability, TOnOffCapability,
};

#[derive(Clone, Default)]
pub struct Registry(Arc<Mutex<BTreeMap<String, Station>>>);

#[derive(Clone, Default)]
struct Station {
    connection_id: u64,
    hub_endpoint: String,
    capability: Option<TIotDiscoveryCapability>,
    endpoints: BTreeMap<String, Endpoint>,
    session: Option<Session>,
}

#[derive(Clone)]
struct Endpoint {
    endpoint: TEndpoint,
    on_off: Option<TOnOffCapability>,
    brightness: Option<TLevelCapability>,
}

#[derive(Clone, Serialize)]
struct Session {
    id: String,
    protocol: String,
    expires_at: i64,
    status: &'static str,
    callback_id: Option<u32>,
    candidates: Vec<Value>,
    message: String,
    #[serde(skip)]
    system_info: String,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Refresh,
    Start {
        protocol: String,
        setup_code: Option<String>,
        ssid: Option<String>,
        password: Option<String>,
    },
    Finish {
        session_id: String,
        callback_id: u32,
        endpoint_id: String,
    },
    Cancel {
        session_id: String,
    },
    OnOff {
        endpoint_id: String,
        on: bool,
    },
    Brightness {
        endpoint_id: String,
        value: f64,
    },
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The station supports a separate read-only environment snapshot option;
/// requesting ordinary player/device state does not include these endpoints.
pub fn refresh_message() -> crate::proto::n_alice::n_alice_api::TServerMessage {
    use crate::proto::n_alice::n_alice_api::{
        t_server_message, t_sync_request, TEventHeader, TGetDeviceState, TServerMessage, TSyncPush,
        TSyncRequest,
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
                        get_environment_state_options: Some(Default::default()),
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

impl Registry {
    pub fn disconnected(&self, duid: &str, connection_id: u64) {
        let mut stations = self.0.lock().unwrap();
        if stations
            .get(duid)
            .is_some_and(|station| station.connection_id == connection_id)
        {
            stations.remove(duid);
        }
    }

    pub fn status(&self, duid: &str) -> Value {
        let mut stations = self.0.lock().unwrap();
        let Some(station) = stations.get_mut(duid) else {
            return json!({"capability_received":false,"protocols":[],"endpoints":[],"session":null});
        };
        station.expire();
        let protocols: Vec<_> = [("zigbee", discovery::TProtocol::Zigbee), ("matter", discovery::TProtocol::Matter)]
            .into_iter().map(|(name, protocol)| {
                let capability = station.capability.as_ref();
                let supported = capability.and_then(|c| c.parameters.as_ref()).is_some_and(|p| p.supported_protocols.contains(&(protocol as i32)));
                let status = capability.and_then(|c| c.state.as_ref()).and_then(|s| s.statuses.iter().find(|s| s.protocol == protocol as i32))
                    .and_then(|s| discovery::EStatus::try_from(s.status).ok()).unwrap_or(discovery::EStatus::Unknown);
                json!({"id":name,"supported":supported,"status":status.as_str_name(),"can_pair":supported && status == discovery::EStatus::Ready && station.supports(EDirectiveType::IotStartDiscoveryDirectiveType)})
            }).collect();
        json!({"capability_received":station.capability.is_some(),"protocols":protocols,
            "endpoints":station.endpoints.values().map(Endpoint::view).collect::<Vec<_>>(),"session":station.session})
    }

    /// State changes commit only after the command was queued to the connection
    /// that supplied the capabilities. Callers must not hold a registry lock.
    pub fn dispatch(
        &self,
        duid: &str,
        action: Action,
        send: impl FnOnce(u64, TDirective) -> bool,
    ) -> Result<(), &'static str> {
        let mut stations = self.0.lock().unwrap();
        let station = stations
            .get_mut(duid)
            .ok_or("Колонка ещё не сообщила о встроенном хабе")?;
        station.expire();
        let mut next = station.clone();
        let directive = next.prepare(action)?;
        if !send(station.connection_id, directive) {
            return Err("Подключение колонки изменилось. Обновите состояние хаба");
        }
        *station = next;
        Ok(())
    }
}

impl Station {
    fn supports(&self, directive: EDirectiveType) -> bool {
        self.capability
            .as_ref()
            .and_then(|c| c.meta.as_ref())
            .is_some_and(|m| m.supported_directives.contains(&(directive as i32)))
    }

    fn expire(&mut self) {
        if let Some(session) = self.session.as_mut().filter(|s| {
            s.expires_at < now()
                && matches!(
                    s.status,
                    "queued" | "searching" | "awaiting_confirmation" | "confirming" | "cancelling"
                )
        }) {
            session.status = "expired";
            session.callback_id = None;
            session.message =
                "Время ожидания истекло. Завершение подключения станцией не подтверждено.".into();
        }
    }

    fn prepare(&mut self, action: Action) -> Result<TDirective, &'static str> {
        match action {
            Action::Refresh => Err("Запрос состояния выполняется отдельно от команд управления"),
            Action::Start {
                protocol,
                setup_code,
                ssid,
                password,
            } => {
                if self.session.as_ref().is_some_and(|s| {
                    matches!(
                        s.status,
                        "queued"
                            | "searching"
                            | "awaiting_confirmation"
                            | "confirming"
                            | "cancelling"
                    )
                }) {
                    return Err("Сначала завершите или отмените текущий поиск");
                }
                let kind = match protocol.as_str() {
                    "zigbee" => discovery::TProtocol::Zigbee,
                    "matter" => discovery::TProtocol::Matter,
                    _ => return Err("Поддерживаются Zigbee и Matter over Wi-Fi"),
                };
                let capability = self
                    .capability
                    .as_ref()
                    .ok_or("Нет данных о возможностях хаба")?;
                if !self.supports(EDirectiveType::IotStartDiscoveryDirectiveType)
                    || !capability
                        .parameters
                        .as_ref()
                        .is_some_and(|p| p.supported_protocols.contains(&(kind as i32)))
                {
                    return Err("Станция не сообщила о поддержке этого протокола");
                }
                if !capability.state.as_ref().is_some_and(|s| {
                    !s.is_discovery_in_progress
                        && s.statuses.iter().any(|s| {
                            s.protocol == kind as i32
                                && s.status == discovery::EStatus::Ready as i32
                        })
                }) {
                    return Err("Хаб ещё не готов к подключению устройств");
                }
                use discovery::t_protocol_parameters::{self as parameters, Parameters};
                let parameter = if kind == discovery::TProtocol::Zigbee {
                    Parameters::ZigbeeParameters(parameters::TZigbeeParameters { device_limit: 1 })
                } else {
                    let setup_code = setup_code.unwrap_or_default().trim().to_owned();
                    if setup_code.len() > 256
                        || !(setup_code.starts_with("MT:") && setup_code.len() > 3
                            || [11, 21].contains(&setup_code.len())
                                && setup_code.bytes().all(|b| b.is_ascii_digit()))
                    {
                        return Err(
                            "Нужен код Matter из 11 или 21 цифры либо содержимое QR-кода MT:",
                        );
                    }
                    let ssid = ssid.unwrap_or_default();
                    let password = password.unwrap_or_default();
                    if ssid.is_empty()
                        || ssid.len() > 32
                        || password.len() > 64
                        || ssid.contains('\0')
                        || password.contains('\0')
                    {
                        return Err(
                            "Укажите сеть Wi-Fi 2,4 ГГц: SSID до 32 байт и пароль до 64 байт",
                        );
                    }
                    // V3 describes transport explicitly; older firmware uses V1.
                    if capability
                        .parameters
                        .as_ref()
                        .is_some_and(|p| p.matter_parameters_ver >= 3)
                    {
                        Parameters::MatterParametersV3(parameters::TMatterParametersV3 {
                            devices_parameters: vec![parameters::TDeviceParameters {
                                setup_code_payload: setup_code,
                                transport: discovery::TMatterTransport::Wifi as i32,
                            }],
                            transports_parameters: vec![parameters::TTransportParameters {
                                transport: discovery::TMatterTransport::Wifi as i32,
                                parameters: Some(
                                    parameters::t_transport_parameters::Parameters::WiFiParameters(
                                        parameters::TWiFiTransportParameters { ssid, password },
                                    ),
                                ),
                            }],
                        })
                    } else {
                        Parameters::MatterParameters(parameters::TMatterParameters {
                            setup_code_payload: setup_code,
                            ssid,
                            password,
                        })
                    }
                };
                let id = uuid::Uuid::new_v4().to_string();
                let name = "iot_start_discovery_directive";
                let command = directive(
                    name,
                    EDirectiveType::IotStartDiscoveryDirectiveType,
                    &self.hub_endpoint,
                    discovery::TStartDiscoveryDirective {
                        name: name.into(),
                        protocols: vec![kind as i32],
                        timeout_ms: 120_000,
                        device_limit: 1,
                        protocol_parameters: vec![discovery::TProtocolParameters {
                            protocol: kind as i32,
                            parameters: Some(parameter),
                        }],
                        session_id: id.clone(),
                        mode: discovery::EMode::Interactive as i32,
                    },
                );
                self.session = Some(Session {
                    id,
                    protocol,
                    expires_at: now() + 150,
                    status: "queued",
                    callback_id: None,
                    candidates: Vec::new(),
                    message: "Команда поиска отправлена. Ожидаем ответ станции.".into(),
                    system_info: String::new(),
                });
                Ok(command)
            }
            Action::Cancel { session_id } => {
                if !self.supports(EDirectiveType::IotCancelDiscoveryDirectiveType) {
                    return Err("Станция не сообщила о поддержке отмены поиска");
                }
                let session = self
                    .session
                    .as_mut()
                    .filter(|s| s.id == session_id && s.expires_at >= now())
                    .ok_or("Текущий сеанс поиска не найден")?;
                session.status = "cancelling";
                session.callback_id = None;
                session.message = "Отмена отправлена. Ожидаем ответ станции.".into();
                let name = "iot_cancel_discovery_directive";
                Ok(directive(
                    name,
                    EDirectiveType::IotCancelDiscoveryDirectiveType,
                    &self.hub_endpoint,
                    discovery::TCancelDiscoveryDirective {
                        name: name.into(),
                        session_id,
                    },
                ))
            }
            Action::Finish {
                session_id,
                callback_id,
                endpoint_id,
            } => {
                if !self.supports(EDirectiveType::IotFinishDiscoveryDirectiveType) {
                    return Err("Станция не сообщила о поддержке подтверждения устройства");
                }
                let session = self
                    .session
                    .as_mut()
                    .filter(|s| {
                        s.id == session_id
                            && s.callback_id == Some(callback_id)
                            && s.status == "awaiting_confirmation"
                            && s.expires_at >= now()
                    })
                    .ok_or("Подтверждение устарело. Запустите поиск заново")?;
                let candidate = session
                    .candidates
                    .iter()
                    .find(|e| e["id"] == endpoint_id)
                    .ok_or("Устройства нет в текущем ответе станции")?;
                let protocol = candidate["protocol_code"]
                    .as_i64()
                    .ok_or("Станция не указала протокол устройства")?
                    as i32;
                let name = "iot_finish_discovery_directive";
                let command = directive(
                    name,
                    EDirectiveType::IotFinishDiscoveryDirectiveType,
                    &self.hub_endpoint,
                    discovery::TFinishDiscoveryDirective {
                        name: name.into(),
                        accepted_ids: Vec::new(),
                        accepted_endpoints: vec![discovery::TEndpointInfo {
                            id: endpoint_id,
                            protocol,
                        }],
                        system_info: session.system_info.clone(),
                        callback_id,
                    },
                );
                session.status = "confirming";
                session.callback_id = None;
                session.message =
                    "Подтверждение отправлено. Ожидаем завершения подключения станцией.".into();
                Ok(command)
            }
            Action::OnOff { endpoint_id, on } => {
                let endpoint = self
                    .endpoints
                    .get(&endpoint_id)
                    .ok_or("Устройство не найдено в актуальном состоянии станции")?;
                if !endpoint
                    .on_off
                    .as_ref()
                    .and_then(|c| c.meta.as_ref())
                    .is_some_and(|m| {
                        m.supported_directives
                            .contains(&(EDirectiveType::OnOffDirectiveType as i32))
                    })
                {
                    return Err("Устройство не сообщило о поддержке включения и выключения");
                }
                let name = "on_off_directive";
                Ok(directive(
                    name,
                    EDirectiveType::OnOffDirectiveType,
                    &endpoint_id,
                    crate::proto::n_alice::t_on_off_capability::TOnOffDirective {
                        name: name.into(),
                        on,
                    },
                ))
            }
            Action::Brightness { endpoint_id, value } => {
                let capability = self
                    .endpoints
                    .get(&endpoint_id)
                    .and_then(|e| e.brightness.as_ref())
                    .ok_or("Устройство не сообщило о регулировке яркости")?;
                let range = capability
                    .parameters
                    .as_ref()
                    .and_then(|p| p.range.as_ref())
                    .ok_or("Устройство не сообщило допустимый диапазон яркости")?;
                if !value.is_finite()
                    || value < range.min
                    || value > range.max
                    || !capability.meta.as_ref().is_some_and(|m| {
                        m.supported_directives
                            .contains(&(EDirectiveType::SetAbsoluteLevelDirectiveType as i32))
                    })
                {
                    return Err(
                        "Яркость вне диапазона устройства или управление не поддерживается",
                    );
                }
                let name = "set_absolute_level_directive";
                Ok(directive(
                    name,
                    EDirectiveType::SetAbsoluteLevelDirectiveType,
                    &endpoint_id,
                    crate::proto::n_alice::t_level_capability::TSetAbsoluteLevelDirective {
                        name: name.into(),
                        target_level: value,
                        transition_time: 0,
                        with_on_off: true,
                    },
                ))
            }
        }
    }
}

impl Endpoint {
    fn new(endpoint: TEndpoint) -> Option<Self> {
        Self::reported(endpoint, None)
    }

    fn reported(mut endpoint: TEndpoint, previous: Option<&Self>) -> Option<Self> {
        if let Some(previous) = previous {
            if endpoint.meta.is_none() {
                endpoint.meta = previous.endpoint.meta.clone();
            }
            if let (Some(meta), Some(old)) = (&mut endpoint.meta, &previous.endpoint.meta) {
                if meta.device_info.is_none() {
                    meta.device_info = old.device_info.clone();
                }
            }
            if endpoint.status.is_none() {
                endpoint.status = previous.endpoint.status;
            }
        }
        let protocol = endpoint.meta.as_ref()?.device_info.as_ref()?.protocol;
        if ![1, 2].contains(&protocol) || endpoint.id.is_empty() {
            return None;
        }
        let mut result = Self {
            endpoint,
            on_off: previous.and_then(|p| p.on_off.clone()),
            brightness: previous.and_then(|p| p.brightness.clone()),
        };
        for holder in result.endpoint.any_capabilities.clone() {
            result.capability(holder.capability_type, &holder.raw_capability);
        }
        for capability in result.endpoint.capabilities.clone() {
            let kind = match capability.type_url.rsplit('/').next() {
                Some("NAlice.TOnOffCapability") => ECapabilityType::OnOffCapabilityType,
                Some("NAlice.TLevelCapability") => ECapabilityType::LevelCapabilityType,
                _ => continue,
            };
            result.capability(kind as i32, &capability.value);
        }
        Some(result)
    }

    fn capability(&mut self, kind: i32, raw: &[u8]) {
        if kind == ECapabilityType::OnOffCapabilityType as i32 {
            if let Ok(update) = TOnOffCapability::decode(raw) {
                let c = self.on_off.get_or_insert_default();
                if update.meta.is_some() {
                    c.meta = update.meta;
                }
                if update.parameters.is_some() {
                    c.parameters = update.parameters;
                }
                if update.state.is_some() {
                    c.state = update.state;
                }
            }
        } else if kind == ECapabilityType::LevelCapabilityType as i32 {
            if let Ok(update) = TLevelCapability::decode(raw) {
                if update.parameters.as_ref().is_some_and(|p| {
                    p.instance
                        != crate::proto::n_alice::t_level_capability::EInstance::BrightnessInstance
                            as i32
                }) {
                    return;
                }
                if update.parameters.is_none() && self.brightness.is_none() {
                    return;
                }
                let c = self.brightness.get_or_insert_default();
                if update.meta.is_some() {
                    c.meta = update.meta;
                }
                if update.parameters.is_some() {
                    c.parameters = update.parameters;
                }
                if update.state.is_some() {
                    c.state = update.state;
                }
            }
        }
    }

    fn view(&self) -> Value {
        let info = self
            .endpoint
            .meta
            .as_ref()
            .and_then(|m| m.device_info.as_ref());
        let brightness = self.brightness.as_ref().and_then(|c| Some(json!({"value":c.state.as_ref().map(|s|s.level),
            "min":c.parameters.as_ref()?.range.as_ref()?.min,"max":c.parameters.as_ref()?.range.as_ref()?.max,
            "step":c.parameters.as_ref()?.range.as_ref()?.precision,
            "writable":c.meta.as_ref().is_some_and(|m|m.supported_directives.contains(&(EDirectiveType::SetAbsoluteLevelDirectiveType as i32)))})));
        json!({"id":self.endpoint.id,"manufacturer":info.map(|i|&i.manufacturer),"model":info.map(|i|&i.model),
            "protocol":if info.is_some_and(|i|i.protocol == 1) { "zigbee" } else { "matter" },"protocol_code":info.map(|i|i.protocol),
            "online":self.endpoint.status.as_ref().map(|s|s.status == 2),
            "on":self.on_off.as_ref().and_then(|c|c.state.as_ref()).map(|s|s.on),
            "can_toggle":self.on_off.as_ref().and_then(|c|c.meta.as_ref()).is_some_and(|m|m.supported_directives.contains(&(EDirectiveType::OnOffDirectiveType as i32))),"brightness":brightness})
    }
}

fn directive(
    name: &str,
    kind: EDirectiveType,
    endpoint: &str,
    payload: impl Message,
) -> TDirective {
    TDirective {
        name: name.into(),
        r#type: "client_action".into(),
        directive_type: kind as i32,
        endpoint_id: (!endpoint.is_empty()).then(|| StringValue {
            value: endpoint.into(),
        }),
        payload_type: Some(PayloadType::PayloadRaw(payload.encode_to_vec())),
        is_led_silent: true,
        ..Default::default()
    }
}
