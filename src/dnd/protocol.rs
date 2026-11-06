//! Typed native DND controls. The config's enabled flag enables a daily native
//! schedule; an explicit force directive controls the current physical state.

use prost::Message;

use crate::proto::google::protobuf::StringValue;
use crate::proto::n_alice::n_alice_api::t_directive::PayloadType;
use crate::proto::n_alice::n_alice_api::{TCapabilityUpdateState, TDirective, TEnvironmentState};
use crate::proto::n_alice::t_capability::{ECapabilityType, EDirectiveType};
use crate::proto::n_alice::t_do_not_disturb_capability::{
    t_set_do_not_disturb_state_directive::EForceDoNotDisturbState, TSetDoNotDisturbStateDirective,
};
use crate::proto::n_alice::t_endpoint::{EAccessType, EEndpointType};
use crate::proto::n_alice::TDoNotDisturbCapability;

#[derive(Clone, Default)]
pub struct DndCapability {
    endpoint_id: String,
    capability: TDoNotDisturbCapability,
}

impl DndCapability {
    pub fn matches_endpoint(&self, endpoint_id: &str) -> bool {
        !self.endpoint_id.is_empty() && self.endpoint_id == endpoint_id
    }

    pub fn update(&mut self, endpoint_id: &str, update: TDoNotDisturbCapability) {
        if self.endpoint_id != endpoint_id {
            *self = Self::default();
            self.endpoint_id = endpoint_id.to_owned();
        }
        if update.meta.is_some() {
            self.capability.meta = update.meta;
        }
        if update.parameters.is_some() {
            self.capability.parameters = update.parameters;
        }
        if update.state.is_some() {
            self.capability.state = update.state;
        }
    }

    pub fn reported_enabled(&self) -> Option<bool> {
        self.capability
            .state
            .as_ref()
            .map(|state| state.is_do_not_disturb_enabled)
    }

    pub fn force_directive(&self, enabled: Option<bool>) -> Result<TDirective, &'static str> {
        if !self.capability.meta.as_ref().is_some_and(|meta| {
            meta.supported_directives
                .contains(&(EDirectiveType::SetDoNotDisturbStateDirectiveType as i32))
        }) {
            return Err("Колонка ещё не сообщила о поддержке управления режимом «Не беспокоить»");
        }
        let name = "set_do_not_disturb_directive";
        let force = match enabled {
            None => EForceDoNotDisturbState::NoForceState,
            Some(false) => EForceDoNotDisturbState::ForceDisable,
            Some(true) => EForceDoNotDisturbState::ForceEnable,
        };
        let payload = TSetDoNotDisturbStateDirective {
            name: name.to_owned(),
            force_do_not_disturb_state: force as i32,
        };
        Ok(TDirective {
            name: name.to_owned(),
            r#type: "client_action".to_owned(),
            directive_type: EDirectiveType::SetDoNotDisturbStateDirectiveType as i32,
            endpoint_id: Some(StringValue {
                value: self.endpoint_id.clone(),
            }),
            payload_type: Some(PayloadType::PayloadRaw(payload.encode_to_vec())),
            is_led_silent: true,
            ..Default::default()
        })
    }
}

pub fn capability_update(event: &TCapabilityUpdateState) -> Option<TDoNotDisturbCapability> {
    let kind = ECapabilityType::DoNotDisturbCapabilityType as i32;
    event
        .any_capability
        .as_ref()
        .filter(|holder| {
            (holder.capability_type == kind
                || (holder.capability_type == 0 && event.capability_type == kind))
                && !holder.raw_capability.is_empty()
        })
        .and_then(|holder| TDoNotDisturbCapability::decode(holder.raw_capability.as_slice()).ok())
        .or_else(|| {
            let capability = event.capability.as_ref()?;
            let matches =
                capability.type_url.rsplit('/').next() == Some("NAlice.TDoNotDisturbCapability");
            if (event.capability_type != kind && !matches) || capability.value.is_empty() {
                return None;
            }
            TDoNotDisturbCapability::decode(capability.value.as_slice()).ok()
        })
        .filter(|capability| {
            capability.meta.is_some()
                || capability.parameters.is_some()
                || capability.state.is_some()
        })
}

pub fn environment_capability<'a>(
    environment: &'a TEnvironmentState,
    duid: &str,
) -> Option<(&'a str, TDoNotDisturbCapability)> {
    let mut reported = environment
        .endpoints
        .iter()
        .take(256)
        .filter_map(|endpoint| {
            let own = endpoint.id == duid
                || endpoint.meta.as_ref().is_some_and(|meta| {
                    meta.r#type == EEndpointType::SpeakerEndpointType as i32
                        && meta.access_type == EAccessType::Embedded as i32
                });
            if !own {
                return None;
            }
            let raw = endpoint
                .any_capabilities
                .iter()
                .find(|holder| {
                    holder.capability_type == ECapabilityType::DoNotDisturbCapabilityType as i32
                        && !holder.raw_capability.is_empty()
                })
                .map(|holder| holder.raw_capability.as_slice())
                .or_else(|| {
                    endpoint
                        .capabilities
                        .iter()
                        .find(|capability| {
                            capability.type_url.rsplit('/').next()
                                == Some("NAlice.TDoNotDisturbCapability")
                                && !capability.value.is_empty()
                        })
                        .map(|capability| capability.value.as_slice())
                })?;
            let capability = TDoNotDisturbCapability::decode(raw).ok()?;
            (capability.meta.is_some()
                || capability.parameters.is_some()
                || capability.state.is_some())
            .then_some((endpoint.id.as_str(), capability))
        });
    let result = reported.next()?;
    reported.next().is_none().then_some(result)
}
