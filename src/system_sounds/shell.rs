use super::{DIRECTORY, ORIGINALS};
use crate::device_access::operations::quote;
use base64::{engine::general_purpose::STANDARD, Engine};

pub(super) const LIST: &str = r#"
D=/data/quasar/quasar-panel/sounds
for group in common ru-RU; do
    for path in "/system/vendor/quasar/sounds/$group/"*; do
        [ -f "$path" ] && [ ! -L "$path" ] || continue
        name=${path##*/}
        custom=0 persistent=0
        source="$D/files/$group/$name"
        if [ -f "$source" ] && [ "$(stat -c '%d:%i' "$source")" = "$(stat -c '%d:%i' "$path")" ]; then custom=1; fi
        [ "$(cat "$D/entries/$group/$name" 2>/dev/null)" != 1 ] || persistent=1
        printf '%s|%s|%s|%s|%s\n' "$group" "$name" "$(wc -c < "$path")" "$custom" "$persistent"
    done
done
"#;

const BOOT: &str = include_str!("boot.sh");

fn common() -> String {
    format!(
        r##"set -e
umask 077
D={DIRECTORY}
O={ORIGINALS}
ROOT=/system/vendor/quasar/sounds
KEEPER=/data/quasar/rootkeeper.sh
mkdir /tmp/quasar-system-sounds.lock 2>/dev/null || {{ echo 'Менеджер звуков занят'; exit 2; }}
rootlock=0
cleanup() {{ code=$?; rm -f "$KEEPER.sounds-new"; [ "$rootlock" != 1 ] || rmdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null; rmdir /tmp/quasar-system-sounds.lock 2>/dev/null; exit "$code"; }}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM
mkdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null || {{ echo 'Загрузочный файл изменяется другим компонентом'; exit 2; }}
rootlock=1
KEEPER_MISSING=0
if [ -f "$KEEPER" ]; then
[ ! -L "$KEEPER" ] && [ "$(head -n 1 "$KEEPER")" = '#!/bin/sh' ] || {{ echo 'Не подтверждён формат rootkeeper.sh'; exit 3; }}
begins=$(grep -F -c '# BEGIN quasar-system-sounds' "$KEEPER" || true)
ends=$(grep -F -c '# END quasar-system-sounds' "$KEEPER" || true)
[ "$begins" = "$ends" ] && [ "$begins" -le 1 ] || {{ echo 'Повреждён собственный boot-блок звуков'; exit 3; }}
else KEEPER_MISSING=1; fi
mkdir -p "$D/files/common" "$D/files/ru-RU" "$D/entries/common" "$D/entries/ru-RU" "$O/common" "$O/ru-RU" /data/quasar/quasar-panel/backups
BACKUP="/data/quasar/quasar-panel/backups/sounds-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir "$BACKUP"
[ "$KEEPER_MISSING" = 1 ] || cp -p "$KEEPER" "$BACKUP/rootkeeper.sh"
mounted() {{ awk -v p="$1" '$2==p{{count++}} END{{print count+0}}' /proc/mounts; }}
owned() {{ [ -f "$2" ] && [ "$(stat -c '%d:%i' "$1")" = "$(stat -c '%d:%i' "$2")" ]; }}
update_hook() {{
    persistent=0
    for entry in "$D"/entries/common/* "$D"/entries/ru-RU/*; do [ -f "$entry" ] || continue; [ "$(cat "$entry")" != 1 ] || persistent=1; done
    if [ "$KEEPER_MISSING" = 1 ]; then [ "$persistent" = 0 ] || {{ echo 'Нет rootkeeper для сохранения автозапуска'; return 1; }}; return 0; fi
    awk 'BEGIN{{block=0}} $0=="# BEGIN quasar-system-sounds"{{block=1;next}} $0=="# END quasar-system-sounds"{{block=0;next}} !block{{print}}' "$KEEPER" > "$D/keeper.clean" || return 1
    if [ "$persistent" = 1 ]; then
        awk 'NR==1{{print;print "# BEGIN quasar-system-sounds";print "sh {DIRECTORY}/boot.sh >/dev/null 2>&1";print "# END quasar-system-sounds";next}}{{print}}' "$D/keeper.clean" > "$KEEPER.sounds-new" || return 1
    else cp "$D/keeper.clean" "$KEEPER.sounds-new" || return 1; fi
    sh -n "$KEEPER.sounds-new" || return 1
    chmod 755 "$KEEPER.sounds-new" || return 1
    mv "$KEEPER.sounds-new" "$KEEPER" || return 1
    rm -f "$D/keeper.clean"
}}
"##
    )
}

pub(super) fn replace(
    group: &str,
    name: &str,
    incoming: &str,
    token: &str,
    persistent: bool,
) -> String {
    let mut script = common();
    script.push_str(&format!(r#"TARGET="$ROOT/{group}/{name}"
FILE="$D/files/{group}/{name}"
ENTRY="$D/entries/{group}/{name}"
ORIGINAL="$O/{group}/{name}"
INCOMING={incoming}
[ -f "$TARGET" ] && [ ! -L "$TARGET" ] || {{ echo 'Исходный системный файл отсутствует'; exit 4; }}
[ -f "$INCOMING" ] || {{ echo 'Загруженный файл отсутствует'; exit 4; }}
count=$(mounted "$TARGET")
[ "$count" -le 1 ] || {{ echo 'У файла несколько mount-слоёв; изменения остановлены'; exit 4; }}
if [ "$count" = 1 ]; then owned "$TARGET" "$FILE" && [ -f "$ORIGINAL" ] || {{ echo 'Файл подменён другим компонентом; он не изменён'; exit 4; }}; fi
"#,incoming=quote(incoming)));
    if persistent {
        script.push_str(r#"[ "$KEEPER_MISSING" = 0 ] && [ -x "$KEEPER" ] && sed '/^[[:space:]]*#/d' /system/vendor/quasar/onboot.sh | grep -F '/data/quasar/rootkeeper.sh' >/dev/null && grep -F '/etc/init.d/rcS' /etc/inittab >/dev/null && grep -F '/system/vendor/quasar/onboot.sh' /etc/init.d/S97quasar >/dev/null && grep -E 'S[?][?]|S\*' /etc/init.d/rcS >/dev/null || { echo 'Не подтверждён автозапуск звуков при загрузке'; exit 5; }
"#);
    }
    script.push_str(&format!(r#"[ -f "$ORIGINAL" ] || {{ cp -p "$TARGET" "$ORIGINAL.new"; mv "$ORIGINAL.new" "$ORIGINAL"; }}
cp -p "$TARGET" "$BACKUP/previous-{group}-{name}"
[ ! -f "$FILE" ] || cp -p "$FILE" "$BACKUP/overlay"
[ ! -f "$ENTRY" ] || cp -p "$ENTRY" "$BACKUP/entry"
printf '%s' {boot} | base64 -d > "$D/boot.sh.new"
mv "$D/boot.sh.new" "$D/boot.sh"
chmod 700 "$D/boot.sh"
[ "$count" != 1 ] || umount "$TARGET"
mv "$INCOMING" "$FILE"
if ! mount --bind "$FILE" "$TARGET"; then
    if [ -f "$BACKUP/overlay" ]; then cp -p "$BACKUP/overlay" "$FILE"; [ "$count" != 1 ] || mount --bind "$FILE" "$TARGET"; else rm -f "$FILE"; fi
    echo 'Bind mount не удался; оригинальный файл сохранён'; exit 6
fi
printf '%s\n' {persistent} > "$ENTRY.new"
mv "$ENTRY.new" "$ENTRY"
if ! update_hook; then
    umount "$TARGET"
    if [ -f "$BACKUP/overlay" ]; then cp -p "$BACKUP/overlay" "$FILE"; [ "$count" != 1 ] || mount --bind "$FILE" "$TARGET"; else rm -f "$FILE"; fi
    if [ -f "$BACKUP/entry" ]; then cp -p "$BACKUP/entry" "$ENTRY"; else rm -f "$ENTRY"; fi
    echo 'Автозапуск не сохранён; предыдущая подмена восстановлена'; exit 7
fi
printf 'Звук заменён. Резервная копия: %s. Оригинальный файл прошивки не перезаписан.\n' "$BACKUP"
# transaction {token}
"#,boot=quote(&STANDARD.encode(BOOT)),persistent=u8::from(persistent)));
    script
}

pub(super) fn restore(group: &str, name: &str) -> String {
    let mut script = common();
    script.push_str(&format!(
        r#"TARGET="$ROOT/{group}/{name}"
FILE="$D/files/{group}/{name}"
ENTRY="$D/entries/{group}/{name}"
count=$(mounted "$TARGET")
[ "$count" -le 1 ] || {{ echo 'У файла несколько mount-слоёв'; exit 4; }}
if [ "$count" = 1 ]; then
    owned "$TARGET" "$FILE" || {{ echo 'Чужая подмена оставлена без изменений'; exit 4; }}
    cp -p "$FILE" "$BACKUP/{group}-{name}"
    umount "$TARGET"
fi
[ ! -f "$ENTRY" ] || mv "$ENTRY" "$BACKUP/entry-{group}-{name}"
[ ! -f "$FILE" ] || mv "$FILE" "$BACKUP/overlay-{group}-{name}"
update_hook
echo 'Подмена снята. Прошивка снова читает оригинальный звук; резервные копии сохранены.'
"#
    ));
    script
}

pub(super) fn cleanup() -> String {
    let mut script = common();
    script.push_str(r#"for group in common ru-RU; do
    for entry in "$D/entries/$group/"*; do
        [ -f "$entry" ] || continue
        name=${entry##*/}
        case "$name" in ''|.*|*[!a-zA-Z0-9_.-]*) echo 'Неожиданное имя собственного entry'; exit 4;; esac
        target="$ROOT/$group/$name" file="$D/files/$group/$name"
        count=$(mounted "$target")
        [ "$count" -le 1 ] || { echo 'Несколько mount-слоёв'; exit 4; }
        if [ "$count" = 1 ]; then owned "$target" "$file" || { echo 'Чужая подмена оставлена без изменений'; exit 4; }; umount "$target"; fi
        mv "$entry" "$BACKUP/entry-$group-$name"
        [ ! -f "$file" ] || mv "$file" "$BACKUP/overlay-$group-$name"
    done
done
update_hook
[ ! -f "$D/boot.sh" ] || mv "$D/boot.sh" "$BACKUP/boot.sh"
echo 'Сняты только собственные подмены звуков. Резервные копии сохранены.'
"#);
    script
}

pub(super) fn apply_saved() -> String {
    let mut script = common();
    script.push_str(r#"for entry in "$D"/entries/common/* "$D"/entries/ru-RU/*; do
    [ -f "$entry" ] && [ "$(cat "$entry")" = 1 ] || continue
    [ "$KEEPER_MISSING" = 0 ] && [ -x "$KEEPER" ] && sed '/^[[:space:]]*#/d' /system/vendor/quasar/onboot.sh | grep -F '/data/quasar/rootkeeper.sh' >/dev/null && grep -F '/etc/init.d/rcS' /etc/inittab >/dev/null && grep -F '/system/vendor/quasar/onboot.sh' /etc/init.d/S97quasar >/dev/null && grep -E 'S[?][?]|S\*' /etc/init.d/rcS >/dev/null || { echo 'Автозапуск сохранённых звуков не подтверждён'; exit 5; }
done
"#);
    script.push_str(&format!("printf '%s' {} | base64 -d > \"$D/boot.sh.new\"\nmv \"$D/boot.sh.new\" \"$D/boot.sh\"\nchmod 700 \"$D/boot.sh\"\n",quote(&STANDARD.encode(BOOT))));
    script.push_str("sh \"$D/boot.sh\" all\nupdate_hook\necho 'Сохранённые подмены применены'\n");
    script
}
