set -e
dir=/data/quasar/quasar-panel/bt-output
expected=@HASH@
model=$(tr -d '\000' < /proc/device-tree/model)
case "$model" in *Cucumber*|*cucumber*) :;; *) exit 2;; esac
[ "$(sha256sum /etc/asound.conf | awk '{print $1}')" = "$expected" ] || exit 3
for binary in dbus-send bluealsad bluealsactl arecord aplay modprobe setsid start-stop-daemon sha256sum mount umount; do
 command -v "$binary" >/dev/null || exit 2
done
[ -f /usr/lib/alsa-lib/libasound_module_pcm_bluealsa.so ] || exit 2
[ -x /system/vendor/quasar/q ] || exit 2
umask 077
mkdir -p "$dir"
mkdir "$dir/setup.lock" 2>/dev/null || exit 4
trap 'rmdir "$dir/setup.lock" 2>/dev/null' EXIT
pid=$(cat "$dir/watch.pid" 2>/dev/null || true)
case "$pid" in ''|*[!0-9]*) :;; *)
 if [ -r "/proc/$pid/cmdline" ] && tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q '/quasar-panel/bt-output/watch.sh'; then exit 4; fi;;
esac
if [ -e "$dir/original.conf" ]; then
 [ "$(sha256sum "$dir/original.conf" | awk '{print $1}')" = "$expected" ] || exit 3
else
 cp /etc/asound.conf "$dir/original.conf"
 printf '%s\n' "$expected" > "$dir/original.sha"
fi
[ "$(sha256sum "$dir/original.conf" | awk '{print $1}')" = "$expected" ] || exit 3
cat "$dir/original.conf" > "$dir/asound.conf"
cat >> "$dir/asound.conf" <<'QUASAR_BLUETOOTH_PCM'

# Quasar Control: temporary Cucumber route; original is kept separately.
pcm.!mixer {
  type dmix
  ipc_key 1976513
  ipc_key_add_uid false
  ipc_perm 0600
  slave {
    pcm "hw:Loopback,0,0"
    format S16_LE
    channels 4
    rate 96000
    buffer_size 15360
    period_size 1920
  }
}
pcm.quasar_bt_capture {
  type plug
  slave {
    pcm "hw:Loopback,1,0"
    format S16_LE
    channels 4
    rate 96000
  }
}
pcm.quasar_bt_output {
  type plug
  slave.pcm "bluealsa:SRV=org.bluealsa.quasar_control,DEV=@MAC@,PROFILE=a2dp"
}
QUASAR_BLUETOOTH_PCM
sha256sum "$dir/asound.conf" | awk '{print $1}' > "$dir/route.sha"
printf '%s\n' '@MAC@' > "$dir/device"
rm -f "$dir/stop"
cat > "$dir/watch.sh" <<'QUASAR_BLUETOOTH_WATCH'
@WATCH@
QUASAR_BLUETOOTH_WATCH
chmod 700 "$dir/watch.sh"
printf 'starting\n' > "$dir/state"
start-stop-daemon -S -b -m -p "$dir/watch.pid" -x /bin/sh -- "$dir/watch.sh"
printf 'started\n'
