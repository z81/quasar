//! Native display presets advertised by the station's animation capability.
//!
//! BinaryAnimation is an opaque, firmware-specific format: it is not an image
//! or a rectangular pixel buffer. In particular, Midi has a segmented clock.
//! Do not send arbitrary image bytes under that envelope.

use prost::Message;
use serde::Serialize;

use crate::proto::n_alice::n_alice_api::t_directive::PayloadType;
use crate::proto::n_alice::n_alice_api::{TCapabilityUpdateState, TDirective};
use crate::proto::n_alice::t_animation_capability::{
    t_draw_animation_directive, t_screen, EAnimationType, EFormat, TDrawAnimationDirective,
};
use crate::proto::n_alice::t_capability::{ECapabilityType, EDirectiveType};
use crate::proto::n_alice::TAnimationCapability;

#[derive(Clone, Default)]
pub struct DisplayCapability {
    endpoint_id: String,
    capability: TAnimationCapability,
}

#[derive(Serialize)]
pub struct Animation {
    pub id: &'static str,
    pub label: &'static str,
    #[serde(skip)]
    kind: EAnimationType,
}

#[derive(Serialize)]
pub struct Screen {
    guid: String,
    screen_type: &'static str,
    family: &'static str,
    enabled: Option<bool>,
}

impl DisplayCapability {
    /// Updates are partial: a state-only report must preserve advertised types.
    /// Reports for another endpoint must never borrow the previous endpoint's
    /// capabilities or routing identity.
    pub fn update(&mut self, endpoint_id: &str, update: TAnimationCapability) {
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

    pub fn animations(&self) -> Vec<Animation> {
        let supports_draw = self.capability.meta.as_ref().is_some_and(|meta| {
            meta.supported_directives
                .contains(&(EDirectiveType::DrawAnimationDirectiveType as i32))
        });
        let Some(parameters) = self.capability.parameters.as_ref().filter(|parameters| {
            supports_draw
                && parameters
                    .supported_formats
                    .contains(&(EFormat::Typed as i32))
        }) else {
            return Vec::new();
        };
        // Each label maps to a documented enum, and only advertised values are
        // exposed. Unknown future enum values are not guessed or dispatched.
        [
            ("smile", "Улыбка", EAnimationType::EmotionalSmile),
            ("joy", "Радость", EAnimationType::EmotionalJoy),
            ("wink", "Подмигивание", EAnimationType::EmotionalWink),
            ("love", "Любовь", EAnimationType::EmotionalLove),
            ("wow", "Удивление", EAnimationType::EmotionalWow),
            ("sadness", "Грусть", EAnimationType::EmotionalSadness),
            ("angry", "Злость", EAnimationType::EmotionalAngry),
            ("pause", "Пауза", EAnimationType::PlayerPause),
            ("next", "Следующий трек", EAnimationType::PlayerNext),
            ("battery", "Заряд батареи", EAnimationType::ShowBatteryLevel),
            ("co2", "Уровень CO₂", EAnimationType::ShowCo2Level),
            ("humidity", "Влажность", EAnimationType::ShowHumidityLevel),
            (
                "temperature",
                "Температура",
                EAnimationType::ShowTemperatureLevel,
            ),
        ]
        .into_iter()
        .filter(|(_, _, kind)| parameters.supported_types.contains(&(*kind as i32)))
        .map(|(id, label, kind)| Animation { id, label, kind })
        .collect()
    }

    pub fn screens(&self) -> Vec<Screen> {
        self.capability
            .parameters
            .as_ref()
            .map_or_else(Vec::new, |parameters| {
                parameters
                    .screens
                    .iter()
                    .map(|screen| Screen {
                        guid: screen.guid.clone(),
                        screen_type: t_screen::EScreenType::try_from(screen.screen_type)
                            .unwrap_or_default()
                            .as_str_name(),
                        family: t_screen::EFamily::try_from(screen.family)
                            .unwrap_or_default()
                            .as_str_name(),
                        enabled: self
                            .capability
                            .state
                            .as_ref()
                            .and_then(|state| {
                                state
                                    .screen_states_map
                                    .iter()
                                    .find(|entry| entry.key == screen.guid)
                            })
                            .and_then(|entry| entry.value.as_ref())
                            .map(|state| state.enabled),
                    })
                    .collect()
            })
    }

    pub fn animation(&self, id: &str, duration_ms: u64) -> Result<TDirective, &'static str> {
        if !(500..=30_000).contains(&duration_ms) {
            return Err("Длительность должна быть от 500 до 30000 мс");
        }
        let animation = self
            .animations()
            .into_iter()
            .find(|animation| animation.id == id)
            .ok_or("Колонка не сообщила о поддержке этой анимации")?;
        use t_draw_animation_directive::{
            t_animation, t_animation_options, EAnimationStopPolicy, ESpeakingAnimationPolicy,
            TAnimation, TAnimationOptions,
        };
        let name = "draw_animation_directive";
        let payload = TDrawAnimationDirective {
            name: name.to_owned(),
            animations: vec![TAnimation {
                value: Some(t_animation::Value::TypedAnimation(
                    t_animation::TTypedAnimation {
                        r#type: animation.kind as i32,
                    },
                )),
            }],
            animation_stop_policy: EAnimationStopPolicy::PlayOnce as i32,
            speaking_animation_policy: ESpeakingAnimationPolicy::SkipSpeakingAnimation as i32,
            animation_options: Some(TAnimationOptions {
                substitution_type: t_animation_options::ESubstitutionType::Foreground as i32,
                min_duration_ms: None,
                max_duration_ms: Some(duration_ms),
            }),
            fade_out_animations: Vec::new(),
        };
        Ok(TDirective {
            name: name.to_owned(),
            r#type: "client_action".to_owned(),
            directive_type: EDirectiveType::DrawAnimationDirectiveType as i32,
            endpoint_id: (!self.endpoint_id.is_empty()).then(|| {
                crate::proto::google::protobuf::StringValue {
                    value: self.endpoint_id.clone(),
                }
            }),
            payload_type: Some(PayloadType::PayloadRaw(payload.encode_to_vec())),
            is_led_silent: true,
            ..Default::default()
        })
    }
}

/// Both holder representations appear in the station protocol. Ignore unrelated
/// and empty envelopes, so they cannot turn an unknown state into support.
pub fn capability_update(event: &TCapabilityUpdateState) -> Option<TAnimationCapability> {
    let animation_type = ECapabilityType::AnimationCapabilityType as i32;
    event
        .any_capability
        .as_ref()
        .filter(|holder| {
            (holder.capability_type == animation_type
                || (holder.capability_type == 0 && event.capability_type == animation_type))
                && !holder.raw_capability.is_empty()
        })
        .and_then(|holder| TAnimationCapability::decode(holder.raw_capability.as_slice()).ok())
        .or_else(|| {
            let capability = event.capability.as_ref()?;
            let matches =
                capability.type_url.rsplit('/').next() == Some("NAlice.TAnimationCapability");
            if (event.capability_type != animation_type && !matches) || capability.value.is_empty()
            {
                return None;
            }
            TAnimationCapability::decode(capability.value.as_slice()).ok()
        })
        .filter(|capability| {
            capability.meta.is_some()
                || capability.parameters.is_some()
                || capability.state.is_some()
        })
}
