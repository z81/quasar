#!/bin/sh
# Reapply only the persistent overlays owned by this component.
D=/data/quasar/quasar-panel/sounds
ROOT=/system/vendor/quasar/sounds
failed=0
for group in common ru-RU; do
    for entry in "$D/entries/$group/"*; do
        [ -f "$entry" ] || continue
        [ "$1" = all ] || [ "$(cat "$entry")" = 1 ] || continue
        name=${entry##*/}
        case "$name" in ''|.*|*[!a-zA-Z0-9_.-]*) continue;; esac
        source="$D/files/$group/$name" target="$ROOT/$group/$name"
        original="/data/quasar/quasar-panel/backups/sounds-original/$group/$name"
        [ -f "$source" ] && [ -f "$target" ] && [ -f "$original" ] && [ ! -L "$target" ] || { echo "Missing sound or original backup: $target" >&2; failed=1; continue; }
        if awk -v p="$target" '$2==p{found=1} END{exit !found}' /proc/mounts; then
            [ "$(stat -c '%d:%i' "$source")" = "$(stat -c '%d:%i' "$target")" ] || { echo "Foreign mount left unchanged: $target" >&2; failed=1; }
            continue
        fi
        mount --bind "$source" "$target" || { echo "Could not apply: $target" >&2; failed=1; }
    done
done
exit "$failed"
