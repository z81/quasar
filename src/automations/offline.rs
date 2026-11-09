use super::AutomationState;
use crate::device_access::operations::{self, quote, Target};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{Offset, TimeZone};
use chrono_tz::OffsetComponents;
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

const DIRECTORY: &str = "/data/quasar/alice-automations";
const DAEMON: &str = include_str!("../../device/automations/alarmd.sh");
const CONTROL: &str = include_str!("../../device/automations/control.sh");
const BEGIN: &str = "# BEGIN quasar-alice-automations";
const END: &str = "# END quasar-alice-automations";

const PROBE: &str = r#"
D=/data/quasar/alice-automations
echo installed=$([ -f "$D/alarmd.sh" ] && echo 1 || echo 0)
echo aplay=$(command -v aplay 2>/dev/null)
echo amixer=$(command -v amixer 2>/dev/null)
if grep -Eq '^[[:space:]]*pcm[.]speakers|^[[:space:]]*pcm[[:space:]]+speakers' /etc/asound.conf 2>/dev/null; then echo suggested_pcm=plug:speakers; else echo suggested_pcm=default; fi
if amixer -D default sget Master 2>/dev/null | grep -q '\[[0-9][0-9]*%\]'; then echo suggested_mixer=Master; fi
if [ -x /data/quasar/rootkeeper.sh ] && [ ! -L /data/quasar/rootkeeper.sh ] &&
   sed '/^[[:space:]]*#/d' /system/vendor/quasar/onboot.sh 2>/dev/null | grep -F '/data/quasar/rootkeeper.sh' >/dev/null &&
   grep -F '/etc/init.d/rcS' /etc/inittab >/dev/null &&
   grep -F '/system/vendor/quasar/onboot.sh' /etc/init.d/S97quasar >/dev/null &&
   grep -E 'S[?][?]|S\*' /etc/init.d/rcS >/dev/null; then echo boot_route=rootkeeper; else echo boot_route=unverified; fi
echo autostart=$(grep -F -c '# BEGIN quasar-alice-automations' /data/quasar/rootkeeper.sh 2>/dev/null)
[ ! -r "$D/runtime.conf" ] || { IFS='|' read -r pcm mixerdev mixer revision < "$D/runtime.conf"; printf 'pcm=%s\nmixer=%s\nrevision=%s\n' "$pcm" "$mixer" "$revision"; }
if [ -f "$D/control.sh" ]; then sh "$D/control.sh" status; else echo running=0; fi
echo clock_epoch=$(date +%s)
echo --EVENTS--
[ ! -r "$D/events.log" ] || tail -n 15 "$D/events.log"
true
"#;

#[derive(Deserialize)]
pub(super) struct StatusQuery {
    host: Option<String>,
    port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    action: String,
    #[serde(default)]
    target: Option<Target>,
    #[serde(default)]
    pcm: Option<String>,
    #[serde(default)]
    mixer: Option<String>,
}

pub(super) async fn status(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Query(query): Query<StatusQuery>,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, false).await {
        return r;
    }
    let supplied = query.host.map(|host| Target {
        host,
        port: query.port.unwrap_or(5555),
    });
    let target = match operations::resolve(&state.api, &duid, supplied).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    answer(operations::execute(&target, PROBE).await)
}

fn answer(result: Result<(i32, Vec<u8>), String>) -> Response {
    match result {
        Ok((0, bytes)) => {
            let output = String::from_utf8_lossy(&bytes);
            let (fields, logs) = output.split_once("--EVENTS--").unwrap_or((&output, ""));
            let values: BTreeMap<_, _> = fields
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .collect();
            Json(json!({"runtime":values, "log":logs.trim(), "message":"Статус прочитан с колонки. Проверка звука не выполнялась."})).into_response()
        }
        Ok((_, bytes)) => operations::error(
            StatusCode::CONFLICT,
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(1800)
                .collect::<String>(),
        ),
        Err(e) => operations::error(StatusCode::BAD_GATEWAY, e),
    }
}

pub(super) async fn operate(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Operation>,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, true).await {
        return r;
    }
    let target = match operations::resolve(&state.api, &duid, input.target).await {
        Ok(t) => t,
        Err(r) => return r,
    };
    let _exclusive = match operations::exclusive(&target).await {
        Ok(guard) => guard,
        Err(error) => return operations::error(StatusCode::CONFLICT, error),
    };
    // Serialize station configuration changes; playback itself has a separate daemon lock.
    let _guard = state.devices.lock().await;
    let script = match input.action.as_str() {
        "install" | "sync" => {
            let document = match state.document(&duid).await { Ok(d) => d, Err(e) => return operations::error(StatusCode::SERVICE_UNAVAILABLE, e) };
            if let Err(e) = document.validate() { return operations::error(StatusCode::UNPROCESSABLE_ENTITY, e); }
            let pcm = input.pcm.unwrap_or_default();
            let mixer = input.mixer.unwrap_or_default();
            if [&pcm, &mixer].iter().any(|v| v.len() > 120 || v.contains(['\n','\r','|','\0'])) { return operations::error(StatusCode::UNPROCESSABLE_ENTITY, "Некорректное имя ALSA PCM или микшера"); }
            match installation(&document, &pcm, &mixer, input.action == "install") { Ok(script) => script, Err(e) => return operations::error(StatusCode::UNPROCESSABLE_ENTITY, e) }
        }
        "start" | "stop" | "snooze" | "shutdown" => format!("set -e\n[ -f {DIRECTORY}/control.sh ] || {{ echo 'Локальный runtime не установлен'; exit 2; }}\nsh {DIRECTORY}/control.sh {}\nsleep 1\n{PROBE}", quote(&input.action)),
        "uninstall" => uninstall(),
        _ => return operations::error(StatusCode::UNPROCESSABLE_ENTITY, "Неизвестная операция локальных будильников"),
    };
    answer(operations::execute(&target, &script).await)
}

fn write_file(script: &mut String, name: &str, bytes: &[u8]) {
    script.push_str(&format!(
        "printf '%s' {} | base64 -d > {}.new\nmv {}.new {}\n",
        quote(&STANDARD.encode(bytes)),
        quote(name),
        quote(name),
        quote(name)
    ));
}

fn installation(
    document: &super::model::Document,
    pcm: &str,
    mixer: &str,
    install: bool,
) -> Result<String, String> {
    let mut script = format!(
        r#"set -e
umask 077
D={DIRECTORY}
for tool in aplay amixer awk base64 date; do command -v "$tool" >/dev/null || {{ echo "Нет необходимой команды: $tool"; exit 2; }}; done
PCM={}
MIXER={}
if [ -z "$PCM" ]; then if grep -Eq '^[[:space:]]*pcm[.]speakers|^[[:space:]]*pcm[[:space:]]+speakers' /etc/asound.conf 2>/dev/null; then PCM=plug:speakers; else PCM=default; fi; fi
[ -n "$MIXER" ] || MIXER=Master
amixer -D default sget "$MIXER" | grep -q '\[[0-9][0-9]*%\]' || {{ echo 'Выберите существующий ALSA playback-регулятор с процентной громкостью'; exit 2; }}
mkdir -p "$D/backups" "$D/tz" "$D/commands"
BACKUP="$D/backups/$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir "$BACKUP"
for file in alarmd.sh control.sh alarms.conf runtime.conf alarm.wav tz; do [ ! -e "$D/$file" ] || cp -pR "$D/$file" "$BACKUP/"; done
"#,
        quote(pcm),
        quote(mixer)
    );
    if install {
        script.push_str(r#"[ -x /data/quasar/rootkeeper.sh ] && [ ! -L /data/quasar/rootkeeper.sh ] || { echo 'Нет проверенного исполняемого rootkeeper.sh'; exit 3; }
sed '/^[[:space:]]*#/d' /system/vendor/quasar/onboot.sh | grep -F '/data/quasar/rootkeeper.sh' >/dev/null || { echo 'Не подтверждён вызов rootkeeper при загрузке'; exit 3; }
grep -F '/etc/init.d/rcS' /etc/inittab >/dev/null && grep -F '/system/vendor/quasar/onboot.sh' /etc/init.d/S97quasar >/dev/null && grep -E 'S[?][?]|S\*' /etc/init.d/rcS >/dev/null || { echo 'Не подтверждена цепочка init.d → onboot'; exit 3; }
"#);
    } else {
        script.push_str(
            "[ -f \"$D/alarmd.sh\" ] || { echo 'Сначала установите локальный runtime'; exit 2; }\n",
        );
    }
    let mut config = String::new();
    for alarm in &document.alarms {
        let zone = alarm
            .timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| "Неизвестный часовой пояс")?;
        write_file(
            &mut script,
            &format!("{DIRECTORY}/tz/{}-{}", document.revision, alarm.id),
            &tzif(zone),
        );
        let expected_offset = chrono::Utc::now()
            .with_timezone(&zone)
            .format("%z")
            .to_string();
        script.push_str(&format!("[ \"$(TZ=:{DIRECTORY}/tz/{}-{} date +%z)\" = {} ] || {{ echo 'Колонка не применила часовой пояс: проверьте системное время и поддержку TZif'; exit 4; }}\n", document.revision, alarm.id, quote(&expected_offset)));
        config.push_str(&format!(
            "{}|{}|{}|{}|{}|{}-{}|{}|{}|{}|{}|{}\n",
            alarm.id,
            u8::from(alarm.enabled),
            alarm.time,
            alarm.weekdays.iter().map(u8::to_string).collect::<String>(),
            alarm.date.as_deref().unwrap_or(""),
            document.revision,
            alarm.id,
            alarm.sound,
            alarm.volume,
            alarm.ramp_seconds,
            alarm.snooze_minutes,
            alarm.timeout_minutes
        ));
        if !alarm.sound.is_empty() {
            script.push_str(&format!("[ -r {0} ] && [ \"$(head -c 4 {0})\" = RIFF ] || {{ echo 'Выбранный звук должен быть доступным локальным WAV'; exit 4; }}\n", quote(&alarm.sound)));
        }
    }
    write_file(
        &mut script,
        &format!("{DIRECTORY}/alarmd.sh"),
        DAEMON.as_bytes(),
    );
    write_file(
        &mut script,
        &format!("{DIRECTORY}/control.sh"),
        CONTROL.as_bytes(),
    );
    write_file(&mut script, &format!("{DIRECTORY}/alarm.wav"), &wave());
    script.push_str(&format!("printf '%s|default|%s|%s\\n' \"$PCM\" \"$MIXER\" {} > \"$D/runtime.conf.new\"\nmv \"$D/runtime.conf.new\" \"$D/runtime.conf\"\n", document.revision));
    write_file(
        &mut script,
        &format!("{DIRECTORY}/alarms.conf"),
        config.as_bytes(),
    );
    script.push_str("chmod 700 \"$D/alarmd.sh\" \"$D/control.sh\"\n");
    if install {
        script.push_str(&format!(r#"mkdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null || {{ echo 'Загрузочный файл сейчас изменяется другим компонентом'; exit 5; }}
trap 'rm -f /data/quasar/rootkeeper.sh.alice-new; rmdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null' EXIT
trap 'exit 130' HUP INT TERM
cp -p /data/quasar/rootkeeper.sh "$BACKUP/rootkeeper.sh"
[ "$(head -n 1 /data/quasar/rootkeeper.sh)" = '#!/bin/sh' ] || {{ echo 'Неожиданный формат rootkeeper.sh'; exit 6; }}
begins=$(grep -F -c '{BEGIN}' /data/quasar/rootkeeper.sh || true)
ends=$(grep -F -c '{END}' /data/quasar/rootkeeper.sh || true)
[ "$begins" = "$ends" ] && [ "$begins" -le 1 ] || {{ echo 'Повреждён блок автозапуска; восстановите резервную копию'; exit 6; }}
awk 'BEGIN {{ block=0 }} $0=="{BEGIN}" {{block=1;next}} $0=="{END}" {{block=0;next}} !block {{print}}' /data/quasar/rootkeeper.sh > "$D/rootkeeper.clean"
awk 'NR==1 {{ print; print "{BEGIN}"; print "sh {DIRECTORY}/control.sh start >/dev/null 2>&1"; print "{END}"; next }} {{print}}' "$D/rootkeeper.clean" > /data/quasar/rootkeeper.sh.alice-new
sh -n /data/quasar/rootkeeper.sh.alice-new
chmod 755 /data/quasar/rootkeeper.sh.alice-new
mv /data/quasar/rootkeeper.sh.alice-new /data/quasar/rootkeeper.sh
rm -f "$D/rootkeeper.clean"
rmdir /tmp/quasar-rootkeeper-edit.lock
trap - EXIT HUP INT TERM
"#));
    }
    // Start after the entire configuration and boot hook have been written.
    script.push_str(&format!("sh \"$D/control.sh\" start\nsleep 1\n{PROBE}"));
    Ok(script)
}

pub(super) fn uninstall() -> String {
    format!(
        r#"set -e
D={DIRECTORY}
[ -f "$D/control.sh" ] || grep -F '{BEGIN}' /data/quasar/rootkeeper.sh >/dev/null 2>&1 || exit 0
[ ! -f "$D/control.sh" ] || sh "$D/control.sh" shutdown
sleep 3
if [ -f "$D/control.sh" ] && sh "$D/control.sh" status | grep -q '^running=1$'; then echo 'Runtime ещё останавливается; повторите удаление'; exit 2; fi
mkdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null || {{ echo 'Загрузочный файл занят'; exit 3; }}
trap 'rm -f /data/quasar/rootkeeper.sh.alice-new; rmdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null' EXIT
trap 'exit 130' HUP INT TERM
mkdir -p "$D/backups"
backup="$D/backups/uninstall-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir "$backup"
if [ -f /data/quasar/rootkeeper.sh ]; then
cp -p /data/quasar/rootkeeper.sh "$backup/rootkeeper.sh"
begins=$(grep -F -c '{BEGIN}' /data/quasar/rootkeeper.sh || true)
ends=$(grep -F -c '{END}' /data/quasar/rootkeeper.sh || true)
[ "$begins" = "$ends" ] && [ "$begins" -le 1 ] || {{ echo 'Повреждён блок автозапуска; требуется восстановление'; exit 6; }}
awk 'BEGIN {{ block=0 }} $0=="{BEGIN}" {{block=1;next}} $0=="{END}" {{block=0;next}} !block {{print}}' /data/quasar/rootkeeper.sh > /data/quasar/rootkeeper.sh.alice-new
sh -n /data/quasar/rootkeeper.sh.alice-new
chmod 755 /data/quasar/rootkeeper.sh.alice-new
mv /data/quasar/rootkeeper.sh.alice-new /data/quasar/rootkeeper.sh
fi
rmdir /tmp/quasar-rootkeeper-edit.lock
trap - EXIT HUP INT TERM
for file in alarmd.sh control.sh; do [ ! -f "$D/$file" ] || mv "$D/$file" "$backup/"; done
rm -f "$D/active" "$D/snooze"
{PROBE}
"#
    )
}

fn wave() -> Vec<u8> {
    // One second, 16 kHz mono PCM. The daemon controls gain through the discovered mixer.
    let rate = 16_000u32;
    let size = rate * 2;
    let mut out = Vec::with_capacity(size as usize + 44);
    out.extend(b"RIFF");
    out.extend((size + 36).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(size.to_le_bytes());
    for sample in 0..rate {
        let time = f64::from(sample) / f64::from(rate);
        let envelope = if time < 0.7 {
            (time / 0.02).min(1.0) * ((0.7 - time) / 0.02).min(1.0)
        } else {
            0.0
        };
        let value = ((time * 880.0 * std::f64::consts::TAU).sin() * 12_000.0 * envelope) as i16;
        out.extend(value.to_le_bytes());
    }
    out
}

fn tzif(zone: chrono_tz::Tz) -> Vec<u8> {
    // Embed IANA transitions so station scheduling never depends on server tzdata or network.
    // TZif v2 uses 64-bit epochs; this covers 2000 through 2100, including DST transitions.
    let start = chrono::Utc
        .with_ymd_and_hms(2000, 1, 1, 0, 0, 0)
        .unwrap()
        .timestamp();
    let end = chrono::Utc
        .with_ymd_and_hms(2101, 1, 1, 0, 0, 0)
        .unwrap()
        .timestamp();
    let info = |timestamp| {
        let local = zone.timestamp_opt(timestamp, 0).unwrap();
        (
            local.offset().fix().local_minus_utc(),
            local.offset().dst_offset().num_seconds() != 0,
        )
    };
    let mut kinds = vec![info(start)];
    let mut transitions = Vec::<(i64, u8)>::new();
    let mut previous = start;
    let mut old = info(start);
    let mut cursor = start + 86400;
    while cursor <= end {
        let current = info(cursor);
        if current != old {
            let (mut low, mut high) = (previous, cursor);
            while high - low > 1 {
                let mid = low + (high - low) / 2;
                if info(mid) == old {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            let index = kinds.iter().position(|v| *v == current).unwrap_or_else(|| {
                kinds.push(current);
                kinds.len() - 1
            });
            transitions.push((high, index as u8));
            old = current;
        }
        previous = cursor;
        cursor += 86400;
    }
    let mut out = Vec::new();
    for wide in [false, true] {
        let selected: Vec<_> = transitions
            .iter()
            .filter(|(time, _)| wide || i32::try_from(*time).is_ok())
            .collect();
        out.extend(b"TZif2");
        out.extend([0; 15]);
        for count in [0, 0, 0, selected.len() as u32, kinds.len() as u32, 8] {
            out.extend(count.to_be_bytes());
        }
        for (time, _) in &selected {
            if wide {
                out.extend(time.to_be_bytes());
            } else {
                out.extend((*time as i32).to_be_bytes());
            }
        }
        for (_, index) in &selected {
            out.push(*index);
        }
        for (offset, dst) in &kinds {
            out.extend(offset.to_be_bytes());
            out.push(u8::from(*dst));
            out.push(if *dst { 4 } else { 0 });
        }
        out.extend(b"STD\0DST\0");
    }
    out.extend(b"\n\n");
    out
}
