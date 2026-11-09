printf '@@cpu1\n'
head -n 1 /proc/stat
sleep 0.2
printf '@@cpu2\n'
head -n 1 /proc/stat
printf '@@memory\n'
cat /proc/meminfo
printf '@@uptime\n'
cat /proc/uptime
printf '@@load\n'
cat /proc/loadavg
printf '@@temperature\n'
for p in /sys/class/thermal/thermal_zone*/temp; do
 [ -r "$p" ] || continue
 cat "$p"
done
printf '@@disk\n'
df -Pk /data 2>/dev/null | tail -n 1
printf '@@wireless\n'
cat /proc/net/wireless 2>/dev/null
printf '@@model\n'
tr -d '\000' < /proc/device-tree/model 2>/dev/null
printf '\n@@kernel\n'
uname -r
printf '@@bluetooth\n'
command -v dbus-send 2>/dev/null
printf '@@logs\n'
for p in /data/quasar/daemons_logs/*; do
 [ -f "$p" ] && [ -r "$p" ] && basename "$p"
done
printf '@@end\n'
true
