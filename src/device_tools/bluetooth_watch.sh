#!/bin/sh
# Quasar's managed Bluetooth route. A live supervisor owns every temporary change.
dir=/data/quasar/quasar-panel/bt-output
read -r mac < "$dir/device" || exit 1
case "$mac" in *[!0-9A-F:]*|'') exit 1;; esac
[ "${#mac}" = 17 ] || exit 1
dev=/org/bluez/hci0/dev_$(printf '%s' "$mac" | tr : _)
applied=0
result=failed
cleanup() {
 trap - EXIT HUP INT TERM
 current=$(sha256sum /etc/asound.conf | awk '{print $1}')
 route=$(cat "$dir/route.sha" 2>/dev/null)
 if [ "$applied" = 1 ] || { [ -n "$route" ] && [ "$current" = "$route" ]; }; then
  if [ "$current" != "$route" ] || ! umount /etc/asound.conf; then
   printf 'restore_failed\n' > "$dir/state"
   exit 1
  fi
  if [ "$(sha256sum /etc/asound.conf | awk '{print $1}')" != "$(cat "$dir/original.sha")" ]; then
   printf 'restore_failed\n' > "$dir/state"
   exit 1
  fi
  /system/vendor/quasar/q restart >/dev/null 2>&1 &
 fi
 pid=$(cat "$dir/forward.pid" 2>/dev/null)
 case "$pid" in ''|*[!0-9]*) :;; *)
  if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q quasar_bt_capture; then
   kill -TERM -"$pid" 2>/dev/null
  fi;;
 esac
 pid=$(cat "$dir/bluealsa.pid" 2>/dev/null)
 case "$pid" in ''|*[!0-9]*) :;; *)
  if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q quasar_control; then
   kill -TERM "$pid" 2>/dev/null
  fi;;
 esac
 rm -f "$dir/watch.pid" "$dir/forward.pid" "$dir/bluealsa.pid"
 printf '%s\n' "$result" > "$dir/state"
}
trap cleanup EXIT
trap 'result=stopped; exit 0' HUP INT TERM
[ ! -e "$dir/stop" ] || { result=stopped; exit 0; }
modprobe snd-aloop || exit 1
start-stop-daemon -S -b -m -p "$dir/bluealsa.pid" -x /usr/bin/bluealsad -- -B quasar_control -p a2dp-source --initial-volume=35 --sbc-quality=high -S || exit 1
ready=0
for n in 1 2 3 4 5; do
 if bluealsactl -B quasar_control status >/dev/null 2>&1; then ready=1; break; fi
 sleep 1
done
[ "$ready" = 1 ] || exit 1
# Establish only the outgoing Audio Sink profile of the chosen paired device.
dbus-send --system --print-reply --reply-timeout=8000 --dest=org.bluez "$dev" org.bluez.Device1.ConnectProfile string:0000110b-0000-1000-8000-00805f9b34fb >/dev/null 2>&1 || exit 1
ready=0
for n in 1 2 3 4 5; do
 if bluealsactl -B quasar_control list-pcms 2>/dev/null | grep -q "dev_$(printf '%s' "$mac" | tr : _)/a2dpsrc/sink"; then ready=1; break; fi
 sleep 1
done
[ "$ready" = 1 ] || exit 1
[ "$(sha256sum /etc/asound.conf | awk '{print $1}')" = "$(cat "$dir/original.sha")" ] || exit 1
[ ! -e "$dir/stop" ] || { result=stopped; exit 0; }
mount --bind "$dir/asound.conf" /etc/asound.conf || exit 1
applied=1
[ "$(sha256sum /etc/asound.conf | awk '{print $1}')" = "$(cat "$dir/route.sha")" ] || exit 1
setsid /bin/sh -c 'arecord -q -D quasar_bt_capture -f S16_LE -c 2 -r 48000 -t raw --buffer-time=160000 | aplay -q -D quasar_bt_output -f S16_LE -c 2 -r 48000 -t raw --buffer-time=200000' > /dev/null 2> "$dir/audio-error.log" &
printf '%s\n' "$!" > "$dir/forward.pid"
/system/vendor/quasar/q restart >/dev/null 2>&1 || exit 1
sleep 1
forward=$(cat "$dir/forward.pid")
kill -0 "$forward" 2>/dev/null || exit 1
printf 'active\n' > "$dir/state"
while [ ! -e "$dir/stop" ]; do
 kill -0 "$forward" 2>/dev/null || exit 1
 connected=$(dbus-send --system --print-reply --reply-timeout=3000 --dest=org.bluez "$dev" org.freedesktop.DBus.Properties.Get string:org.bluez.Device1 string:Connected 2>/dev/null)
 if ! printf '%s' "$connected" | grep -q 'boolean true'; then result=disconnected; exit 0; fi
 sleep 3
done
result=stopped
