printf '@@model\n'
tr -d '\000' < /proc/device-tree/model 2>/dev/null
printf '\n@@clock\n'
for p in /sys/class/leds/cucumber:white:clock-*; do
 [ -r "$p/brightness" ] && [ -r "$p/max_brightness" ] || continue
 name=${p##*clock-}
 IFS= read -r value < "$p/brightness" || continue
 IFS= read -r maximum < "$p/max_brightness" || continue
 printf '%s %s %s\n' "$name" "$value" "$maximum"
done
for n in 0 1; do
 printf '@@fb%s\n' "$n"
 for field in name virtual_size bits_per_pixel stride; do
  printf '%s=' "$field"
  cat "/sys/class/graphics/fb$n/$field" 2>/dev/null
 done
 printf 'format='
 fbset -fb "/dev/fb$n" 2>/dev/null | awk '$1 == "rgba" {print $2}'
 printf '\n@@pixels%s\n' "$n"
 if [ "$n" = 0 ]; then count=72; else count=18; fi
 dd if="/dev/fb$n" bs="$count" count=1 2>/dev/null | od -An -tu1
done
printf '@@end\n'
true
