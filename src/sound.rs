//! Native equalizer state. Configuration is persisted through get_sync_info;
//! reported capability state, never command delivery, confirms the equalizer.

use prost::Message;
use serde_json::{json, Value};

use crate::proto::n_alice::n_alice_api::{TCapabilityUpdateState, TEnvironmentState};
use crate::proto::n_alice::t_capability::ECapabilityType;
use crate::proto::n_alice::t_endpoint::{EAccessType, EEndpointType};
use crate::proto::n_alice::TEqualizerCapability;

pub const BANDS: [(u32, u32); 5] = [
    (60, 90),
    (230, 340),
    (910, 1340),
    (3600, 5200),
    (14000, 13000),
];

#[derive(Clone, Default)]
pub struct SoundCapability {
    endpoint_id: String,
    capability: TEqualizerCapability,
}

impl SoundCapability {
    pub fn update(&mut self, endpoint_id: &str, update: TEqualizerCapability) {
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

    pub fn reported(&self) -> Option<Value> {
        self.capability.state.as_ref().map(|state| {
            json!({
                "enabled": state.preset_mode == 1,
                "preset_mode": state.preset_mode,
                "prevent_clipping": state.prevent_clipping,
                "bands": state.bands.iter().map(|band| json!({
                    "freq":band.frequency, "width":band.width, "gain":band.gain,
                })).collect::<Vec<_>>(),
            })
        })
    }

    pub fn matches(&self, desired: &Value) -> Option<bool> {
        let state = self.capability.state.as_ref()?;
        let enabled = desired["enabled"].as_bool().unwrap_or(false);
        if !enabled {
            return Some(state.preset_mode != 1);
        }
        let bands = desired["bands"].as_array()?;
        Some(
            state.preset_mode == 1
                && state.bands.len() == bands.len()
                && state.bands.iter().zip(bands).all(|(actual, expected)| {
                    [
                        (actual.frequency, "freq"),
                        (actual.width, "width"),
                        (actual.gain, "gain"),
                    ]
                    .iter()
                    .all(|(actual, key)| {
                        expected[*key]
                            .as_f64()
                            .is_some_and(|expected| (actual - expected).abs() < 0.001)
                    })
                }),
        )
    }
}

pub fn capability_update(event: &TCapabilityUpdateState) -> Option<TEqualizerCapability> {
    let kind = ECapabilityType::EqualizerCapabilityType as i32;
    event
        .any_capability
        .as_ref()
        .filter(|holder| {
            (holder.capability_type == kind
                || (holder.capability_type == 0 && event.capability_type == kind))
                && !holder.raw_capability.is_empty()
        })
        .and_then(|holder| TEqualizerCapability::decode(holder.raw_capability.as_slice()).ok())
        .or_else(|| {
            let capability = event.capability.as_ref()?;
            let matches =
                capability.type_url.rsplit('/').next() == Some("NAlice.TEqualizerCapability");
            if (event.capability_type != kind && !matches) || capability.value.is_empty() {
                return None;
            }
            TEqualizerCapability::decode(capability.value.as_slice()).ok()
        })
        .filter(|capability| {
            capability.meta.is_some()
                || capability.parameters.is_some()
                || capability.state.is_some()
        })
}

/// GetDeviceState can return the initial capability only in its environment
/// snapshot. Restrict it to this station's own/embedded speaker endpoint;
/// another speaker in the household must not confirm these settings.
pub fn environment_capability<'a>(
    environment: &'a TEnvironmentState,
    duid: &str,
) -> Option<(&'a str, TEqualizerCapability)> {
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
                    holder.capability_type == ECapabilityType::EqualizerCapabilityType as i32
                        && !holder.raw_capability.is_empty()
                })
                .map(|holder| holder.raw_capability.as_slice())
                .or_else(|| {
                    endpoint
                        .capabilities
                        .iter()
                        .find(|capability| {
                            capability.type_url.rsplit('/').next()
                                == Some("NAlice.TEqualizerCapability")
                                && !capability.value.is_empty()
                        })
                        .map(|capability| capability.value.as_slice())
                })?;
            let capability = TEqualizerCapability::decode(raw).ok()?;
            (capability.state.is_some()
                || capability.parameters.is_some()
                || capability.meta.is_some())
            .then_some((endpoint.id.as_str(), capability))
        });
    let capability = reported.next()?;
    // A snapshot with multiple embedded speakers is ambiguous; wait for a
    // targeted state update instead of displaying the wrong speaker's state.
    reported.next().is_none().then_some(capability)
}

pub fn default_equalizer() -> Value {
    json!({"enabled":false,"smartEnabled":true,"bands":BANDS.iter()
        .map(|(freq,width)|json!({"freq":freq,"width":width,"gain":0})).collect::<Vec<_>>()})
}
