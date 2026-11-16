use crate::device_access::operations as ops;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) const CONFIG: &str = "/system/vendor/quasar/quasar.cfg";
pub(super) const BACKUPS: &str = "/data/quasar/quasar-panel/backups/settings";
const MAX_BYTES: usize = 256 * 1024;

pub(super) async fn read(target: &ops::Target) -> Result<(Vec<u8>, Value), String> {
    // ADB's PTY may translate line endings. Transfer base64 so the optimistic
    // hash and the backup are checked against the exact bytes on disk.
    let script = format!("p={CONFIG}\n[ -f \"$p\" ] && [ ! -L \"$p\" ] || exit 2\n[ \"$(wc -c < \"$p\")\" -le {MAX_BYTES} ] || exit 3\nbase64 < \"$p\"");
    let (code, encoded) = ops::execute(target, &script).await?;
    let bytes = STANDARD.decode(encoded.into_iter().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<_>>()).map_err(|_| "Не удалось передать quasar.cfg")?;
    if code != 0 || bytes.len() > MAX_BYTES { return Err("Не удалось прочитать quasar.cfg (обычный JSON-файл до 256 КиБ)".into()); }
    let config: Value = serde_json::from_slice(&bytes).map_err(|_| "quasar.cfg не является корректным JSON")?;
    if !config.is_object() { return Err("Неизвестный формат quasar.cfg".into()); }
    Ok((bytes, config))
}

pub(super) async fn info(target: &ops::Target) -> Value {
    // Return only names and resolver addresses, never launch arguments, environment or credentials.
    let script = "for p in /system/vendor/quasar/launch.json /etc/resolv.conf; do\nif [ -f \"$p\" ] && [ \"$(wc -c < \"$p\")\" -le 262144 ]; then base64 < \"$p\" | tr -d '\\r\\n'; fi\nprintf '\\n'\ndone";
    let Ok((0, bytes)) = ops::execute(target, script).await else { return json!({"available":false}); };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    let launch = lines.next().and_then(|s| STANDARD.decode(s).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let mut names = Vec::new();
    if let Some(nodes) = launch.as_ref().and_then(|c| c["nodes"].as_array()) {
        for node in nodes {
            if let Some(tasks) = node["tasks"].as_array() {
                for task in tasks {
                    if let Some(name) = task["name"].as_str().filter(|s| s.len() <= 80 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))) { names.push(name.to_owned()); }
                }
            }
        }
    }
    let resolver = lines.next().and_then(|s| STANDARD.decode(s).ok()).and_then(|b| String::from_utf8(b).ok()).map(|s| {
        s.lines().filter_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next() != Some("nameserver") { return None; }
            parts.next()?.parse::<std::net::IpAddr>().ok().map(|a| a.to_string())
        }).take(8).collect::<Vec<_>>()
    });
    json!({"available":true,"services":names,"dns":resolver,"launch_read":launch.as_ref().is_some_and(|c| c["nodes"].is_array())})
}

pub(super) fn write_script(previous: &[u8], next: &[u8], id: &str) -> String {
    let expected = hex::encode(Sha256::digest(previous));
    let digest = hex::encode(Sha256::digest(next));
    let encoded = STANDARD.encode(next);
    let encoded = encoded.as_bytes().chunks(512).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\n");
    format!(r#"set -e
umask 077
P={CONFIG}
B={BACKUPS}
T="$P.quasar-settings-{id}"
root_changed=0
written=0
mountpoint=''
mkdir /tmp/quasar-settings.lock 2>/dev/null || exit 10
cleanup() {{
    status=$?
    rm -f "$T"
    if [ "$root_changed" = 1 ]; then
        if ! mount -o remount,ro "$mountpoint" >/dev/null 2>&1; then
            if [ "$written" = 1 ]; then status=20; else status=21; fi
        fi
    fi
    rmdir /tmp/quasar-settings.lock 2>/dev/null || true
    exit "$status"
}}
trap cleanup EXIT
trap 'exit 19' HUP INT TERM
[ -f "$P" ] && [ ! -L "$P" ] || exit 11
[ "$(sha256sum "$P" | cut -d ' ' -f 1)" = {expected} ] || exit 12
# Refuse file bind mounts and unexpected layouts. Restore exactly the mount mode we found.
mountpoint=$(awk -v p="$P" '$2=="/" || p==$2 || index(p,$2 "/")==1 {{if(length($2)>n){{n=length($2);m=$2}}}} END{{print m}}' /proc/mounts)
case "$mountpoint" in /|/system|/system/vendor) ;; *) exit 13;; esac
mode=$(awk -v m="$mountpoint" '$2==m{{print $4;exit}}' /proc/mounts)
case ",$mode," in *,ro,*) readonly=1;; *,rw,*) readonly=0;; *) exit 13;; esac
[ ! -L "$B" ] || exit 14
mkdir -p "$B"
chmod 700 "$B"
count=$(find "$B" -maxdepth 1 -name 'before-*.json' | wc -l)
[ "$count" -lt 100 ] || exit 15
cp -p "$P" "$B/before-{id}.json"
chmod 600 "$B/before-{id}.json"
[ "$(sha256sum "$B/before-{id}.json" | cut -d ' ' -f 1)" = {expected} ] || exit 14
if [ ! -e "$B/original.json" ]; then cp -p "$B/before-{id}.json" "$B/original.json"; fi
if [ "$readonly" = 1 ]; then
    mount -o remount,rw "$mountpoint" >/dev/null 2>&1 || exit 16
    root_changed=1
fi
cp -p "$P" "$T"
base64 -d > "$T" <<'QUASAR_SETTINGS_DATA'
{encoded}
QUASAR_SETTINGS_DATA
[ "$(sha256sum "$T" | cut -d ' ' -f 1)" = {digest} ] || exit 17
# Catch edits made in another terminal while the backup was being created.
[ "$(sha256sum "$P" | cut -d ' ' -f 1)" = {expected} ] || exit 12
mv -f "$T" "$P"
written=1
sync
"#)
}

pub(super) async fn write(target: &ops::Target, previous: &[u8], next: &Value, id: &str) -> Result<Option<String>, String> {
    let bytes = serde_json::to_vec_pretty(next).map_err(|_| "Не удалось подготовить JSON")?;
    if bytes.len() > MAX_BYTES { return Err("Конфигурация больше 256 КиБ".into()); }
    let (code, _) = ops::execute(target, &write_script(previous, &bytes, id)).await?;
    match code {
        0 => Ok(None),
        20 => Ok(Some("Файл сохранён, но вернуть системный раздел в режим чтения не удалось. Проверьте mount в терминале.".into())),
        10 => Err("Настройки уже изменяются в другой сессии".into()),
        12 => Err("quasar.cfg изменился после чтения. Перечитайте настройки.".into()),
        13 => Err("Неизвестная схема разделов или файл подменён через bind mount; запись остановлена".into()),
        14 => Err("Не удалось создать и проверить резервную копию; запись остановлена".into()),
        15 => Err(format!("Сохранено 100 копий. Скачайте и освободите старые копии в {BACKUPS}")),
        16 => Err("Не удалось открыть системный раздел для записи".into()),
        21 => Err("Запись прервана; вернуть системный раздел в режим чтения не удалось. Проверьте файл и mount в терминале.".into()),
        _ => Err("Запись не завершена. Перечитайте настройки перед повтором; резервная копия сохранена, если прошла подготовка.".into()),
    }
}
