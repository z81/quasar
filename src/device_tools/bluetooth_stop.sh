dir=/data/quasar/quasar-panel/bt-output
[ -d "$dir" ] || exit 0
touch "$dir/stop"
pid=$(cat "$dir/watch.pid" 2>/dev/null)
case "$pid" in ''|*[!0-9]*) :;; *)
 if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q '/quasar-panel/bt-output/watch.sh'; then kill -TERM "$pid"; fi;;
esac
for n in 1 2 3 4 5 6 7 8; do
 state=$(cat "$dir/state" 2>/dev/null)
 case "$state" in stopped|disconnected|failed) break;; restore_failed) exit 3;; esac
 sleep 1
done
# Never race a supervisor still finishing an external command. It owns cleanup.
case "$pid" in ''|*[!0-9]*) :;; *)
 if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q '/quasar-panel/bt-output/watch.sh'; then
  echo 'Supervisor is still stopping' >&2
  exit 3
 fi;;
esac
# Only our exact mount may be removed if a supervisor missed its cleanup.
current=$(sha256sum /etc/asound.conf | awk '{print $1}')
applied=$(cat "$dir/route.sha" 2>/dev/null)
original=$(cat "$dir/original.sha" 2>/dev/null)
# An unrelated mount or edited configuration belongs to its owner.
if [ -z "$original" ] || { [ "$current" != "$original" ] && [ "$current" != "$applied" ]; }; then
 printf 'restore_failed\n' > "$dir/state"
 exit 3
fi
if [ -n "$applied" ] && [ "$current" = "$applied" ]; then
 umount /etc/asound.conf || exit 3
 [ "$(sha256sum /etc/asound.conf | awk '{print $1}')" = "$(cat "$dir/original.sha")" ] || exit 3
 /system/vendor/quasar/q restart >/dev/null 2>&1 &
fi
for entry in forward bluealsa; do
 pid=$(cat "$dir/$entry.pid" 2>/dev/null)
 case "$pid" in ''|*[!0-9]*) continue;; esac
 [ -r "/proc/$pid/cmdline" ] || continue
 command=$(tr '\000' ' ' < "/proc/$pid/cmdline")
 case "$entry:$command" in forward:*quasar_bt_capture*) kill -TERM -"$pid" 2>/dev/null;; bluealsa:*quasar_control*) kill -TERM "$pid" 2>/dev/null;; esac
done
printf 'stopped\n' > "$dir/state"
