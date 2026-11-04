//! Native station alarm directives and reported alarm capability state.
//!
//! Typed alarm directives require protobuf PayloadRaw plus DirectiveType;
//! firmware built with protobuf-lite cannot convert a Struct payload for them.
//! The wire format is documented at https://hackalice.me/internals/directives/.

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Datelike, Offset, Timelike, Utc};
use chrono_tz::Tz;
use prost::Message;

use crate::proto::google::protobuf;
use crate::proto::n_alice::n_alice_api::t_directive::PayloadType;
use crate::proto::n_alice::n_alice_api::{TCapabilityUpdateState, TDirective};
use crate::proto::n_alice::n_data::t_alarm::{t_activation_day, TActivationDay, TDate, TTime};
use crate::proto::n_alice::n_data::t_alarm_sound_settings::{t_media_settings, TMediaSettings};
use crate::proto::n_alice::n_data::{TAlarm, TAlarmSoundSettings, TDefaultSound, TTimezone};
use crate::proto::n_alice::t_alarm_capability::{TAlarmAddDirective, TAlarmCancelDirective};
use crate::proto::n_alice::t_capability::{ECapabilityType, EDirectiveType};
use crate::proto::n_alice::TAlarmCapability;

/// A single occurrence; recurrence and snoozing belong to the persistent scheduler.
pub fn start(run_id: &str, title: &str, timezone: &str, at: DateTime<Utc>) -> Result<TDirective> {
    ensure!(!run_id.is_empty(), "Не задан идентификатор будильника");
    let timezone: Tz = timezone
        .parse()
        .context("Неизвестный часовой пояс будильника")?;
    let local = at.with_timezone(&timezone);
    let alarm = TAlarm {
        alarm_id: run_id.to_owned(),
        title: title.to_owned(),
        local_time: Some(TTime {
            hours: local.hour(),
            minutes: local.minute(),
            seconds: local.second(),
        }),
        timezone: Some(TTimezone {
            timezone_name: timezone.name().to_owned(),
            offset_seconds: i64::from(local.offset().fix().local_minus_utc()),
        }),
        activation_days: vec![TActivationDay {
            activation_day: Some(t_activation_day::ActivationDay::Date(TDate {
                year: u32::try_from(local.year()).context("Некорректная дата будильника")?,
                month: local.month(),
                day: local.day(),
            })),
        }],
        alarm_sound_settings: Some(TAlarmSoundSettings {
            // Leave per-alarm volume unset; only select the native default sound.
            sound_level_info: None,
            media_settings: Some(TMediaSettings {
                media: Some(t_media_settings::Media::DefaultSound(TDefaultSound {
                    sound_type: 0,
                })),
                ..Default::default()
            }),
        }),
        edit_timestamp: u64::try_from(Utc::now().timestamp())
            .context("Некорректное время сервера")?,
        ..Default::default()
    };
    Ok(typed(
        "alarm_add_directive",
        EDirectiveType::AlarmAddDirectiveType,
        TAlarmAddDirective {
            name: "alarm_add_directive".to_owned(),
            alarm: Some(alarm),
        },
        false,
    ))
}

/// Remove this occurrence. Cancellation alone is not a confirmed way to silence
/// an alarm that has already started: use stop() as well for the station's channel.
pub fn cancel(run_id: &str) -> TDirective {
    typed(
        "alarm_cancel_directive",
        EDirectiveType::AlarmCancelDirectiveType,
        TAlarmCancelDirective {
            name: "alarm_cancel_directive".to_owned(),
            alarm_id: run_id.to_owned(),
        },
        true,
    )
}

/// Stops the entire station alarm channel, not a single occurrence ID.
pub fn stop() -> TDirective {
    TDirective {
        name: "alarm_stop".to_owned(),
        r#type: "client_action".to_owned(),
        is_led_silent: true,
        payload_type: Some(PayloadType::Payload(protobuf::Struct::default())),
        ..Default::default()
    }
}

fn typed<M: Message>(
    name: &str,
    directive_type: EDirectiveType,
    payload: M,
    is_led_silent: bool,
) -> TDirective {
    TDirective {
        name: name.to_owned(),
        r#type: "client_action".to_owned(),
        directive_type: directive_type as i32,
        payload_type: Some(PayloadType::PayloadRaw(payload.encode_to_vec())),
        is_led_silent,
        ..Default::default()
    }
}

#[derive(Debug)]
pub struct AlarmState {
    /// None is a metadata-only update; Some(empty) is an explicit empty alarm list.
    pub alarms: Option<Vec<(String, bool)>>,
    /// Whether a nonempty advertised directive list includes native alarm creation.
    /// Omitted metadata/directive lists cannot establish support either way.
    pub supported: Option<bool>,
}

/// Unrelated or malformed capability updates are None, never an empty alarm list.
pub fn capability_state(event: &TCapabilityUpdateState) -> Option<AlarmState> {
    let alarm_type = ECapabilityType::AlarmCapabilityType as i32;
    let capability = event
        .any_capability
        .as_ref()
        .filter(|holder| {
            (holder.capability_type == alarm_type
                || (holder.capability_type == 0 && event.capability_type == alarm_type))
                && !holder.raw_capability.is_empty()
        })
        .and_then(|holder| TAlarmCapability::decode(holder.raw_capability.as_slice()).ok())
        .or_else(|| {
            let capability = event.capability.as_ref()?;
            let type_matches =
                capability.type_url.rsplit('/').next() == Some("NAlice.TAlarmCapability");
            if (event.capability_type != alarm_type && !type_matches) || capability.value.is_empty()
            {
                return None;
            }
            TAlarmCapability::decode(capability.value.as_slice()).ok()
        })?;
    let supported = capability
        .meta
        .filter(|meta| !meta.supported_directives.is_empty())
        .map(|meta| {
            meta.supported_directives
                .contains(&(EDirectiveType::AlarmAddDirectiveType as i32))
        });
    let alarms = capability.state.map(|state| {
        state
            .alarms
            .into_iter()
            .map(|alarm| (alarm.alarm_id, alarm.currently_playing))
            .collect()
    });
    Some(AlarmState { alarms, supported })
}
