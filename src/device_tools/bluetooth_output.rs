//! Opt-in temporary output for the observed Cucumber ALSA layout. The stock
//! Bluetooth sink keeps running; no system file is overwritten and no boot hook
//! is installed. Hardware audio acceptance still requires a user's BT speaker.

use crate::device_access::operations::{self, Target};
use serde_json::{json, Value};

pub(super) const DIRECTORY: &str = "/data/quasar/quasar-panel/bt-output";
const ORIGINAL_HASH: &str = "f75be865339127d81e6a24724a271381c9cb0d548c3a42bc7d93840bc6bae4e3";

pub(super) fn probe_script() -> String {
    format!(
        r#"printf '\n@@output_setup\n'
dir={DIRECTORY}
printf 'original_hash='
if [ -r "$dir/original.sha" ]; then cat "$dir/original.sha"; else sha256sum /etc/asound.conf 2>/dev/null | awk '{{print $1}}'; fi
printf 'current_hash='; sha256sum /etc/asound.conf 2>/dev/null | awk '{{print $1}}'
printf 'active_hash='; cat "$dir/route.sha" 2>/dev/null; echo
printf 'state='; cat "$dir/state" 2>/dev/null; echo
printf 'mac='; cat "$dir/device" 2>/dev/null; echo
printf 'running='
pid=$(cat "$dir/watch.pid" 2>/dev/null)
case "$pid" in ''|*[!0-9]*) echo no;; *)
 if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q '/quasar-panel/bt-output/watch.sh'; then echo yes; else echo no; fi;;
esac
printf 'model='; tr -d '\000' < /proc/device-tree/model 2>/dev/null; echo
printf 'missing='
for binary in dbus-send bluealsad bluealsactl arecord aplay modprobe setsid start-stop-daemon sha256sum mount umount; do
 command -v "$binary" >/dev/null || printf '%s ' "$binary"
done
echo
printf 'loopback='
if grep -q '^snd_aloop ' /proc/modules 2>/dev/null || [ -f /lib/modules/$(uname -r)/kernel/sound/drivers/snd-aloop.ko ]; then echo yes; else echo no; fi
printf 'plugin='
if [ -f /usr/lib/alsa-lib/libasound_module_pcm_bluealsa.so ]; then echo yes; else echo no; fi
printf '@@output_setup_end\n'
true
"#
    )
}

pub(super) fn info(text: &str) -> Value {
    let fields = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<std::collections::BTreeMap<_, _>>();
    let get = |key| fields.get(key).copied().unwrap_or_default().trim();
    let supported = get("model").to_ascii_lowercase().contains("cucumber")
        && get("original_hash") == ORIGINAL_HASH
        && get("missing").is_empty()
        && get("loopback") == "yes"
        && get("plugin") == "yes";
    let active = !get("active_hash").is_empty() && get("current_hash") == get("active_hash");
    let running = get("running") == "yes";
    let state = get("state");
    let reason = if !get("model").to_ascii_lowercase().contains("cucumber") {
        "Переключение полного звука настроено только для Миди (cucumber)"
    } else if get("original_hash") != ORIGINAL_HASH {
        "Конфигурация ALSA отличается от проверенной схемы Миди. Автоматическое переключение недоступно"
    } else if !get("missing").is_empty() {
        "В прошивке отсутствуют необходимые программы для исходящего Bluetooth"
    } else if get("loopback") != "yes" || get("plugin") != "yes" {
        "В прошивке отсутствует ALSA loopback или Bluetooth-плагин"
    } else {
        "Перед включением сохраняется исходная конфигурация. Звуковая служба перезапустится; после отключения Bluetooth-устройства или перезагрузки звук вернётся во встроенный динамик. Проверка с внешней колонкой ещё не выполнена"
    };
    json!({"supported":supported,"active":active,"running":running,"state":state,"mac":get("mac"),"reason":reason,
        "verified_audio":false,"persistent":false,"needs_restore":active&&!running||state=="restore_failed"})
}

pub(super) async fn start(target: &Target, mac: &str) -> Result<(), String> {
    let script = include_str!("bluetooth_prepare.sh")
        .replace("@HASH@", ORIGINAL_HASH)
        .replace("@MAC@", mac)
        .replace("@WATCH@", include_str!("bluetooth_watch.sh"));
    let (code, _) = operations::execute(target, &script).await?;
    match code {
        0 => Ok(()),
        2 => Err("Прошивка не поддерживает необходимый Bluetooth-аудиотракт".into()),
        3 => Err("Конфигурация звука изменилась; исходный маршрут оставлен без изменений".into()),
        4 => Err("Подготовка Bluetooth уже выполняется".into()),
        _ => Err(
            "Не удалось подготовить Bluetooth-выход. Проверьте состояние и восстановление".into(),
        ),
    }
}

/// Used by managed-component cleanup before removing the Bluetooth directory.
pub(crate) async fn stop(target: &Target) -> Result<(), String> {
    let (code, _) = operations::execute(target, include_str!("bluetooth_stop.sh")).await?;
    if code == 0 {
        Ok(())
    } else {
        Err("Не удалось подтвердить восстановление встроенного динамика. Повторите восстановление и проверьте звук".into())
    }
}

/// An active supervisor owns files and a bind mount outside the request lifetime.
pub(super) async fn ensure_idle(target: &Target) -> Result<(), String> {
    let script = r#"dir=/data/quasar/quasar-panel/bt-output
[ -d "$dir" ] || exit 0
[ ! -d "$dir/setup.lock" ] || exit 4
pid=$(cat "$dir/watch.pid" 2>/dev/null)
case "$pid" in ''|*[!0-9]*) :;; *)
 if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q '/quasar-panel/bt-output/watch.sh'; then exit 4; fi;;
esac
original=$(cat "$dir/original.sha" 2>/dev/null)
[ -n "$original" ] || exit 0
[ "$(sha256sum /etc/asound.conf | awk '{print $1}')" = "$original" ] || exit 3
"#;
    let (code, _) = operations::execute(target, script).await?;
    match code {
        0 => Ok(()),
        4 => Err("Сначала верните звук во встроенный динамик и дождитесь завершения Bluetooth-переключения".into()),
        _ => Err("Сначала восстановите встроенный динамик: исходная конфигурация звука не подтверждена".into()),
    }
}
