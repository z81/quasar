//! Short Cucumber ring overlays through its 24-pixel BGR framebuffer.
//! Writing individual LED brightness files can detach the stock LED triggers;
//! framebuffer writes preserve them and let native indication take over.

use std::fmt::Write;

pub(super) fn script(
    animation: &str,
    color: &str,
    brightness: u8,
    duration_ms: u64,
) -> Result<String, String> {
    if !matches!(animation, "wave" | "pulse" | "rainbow") {
        return Err("Неизвестная анимация верхней подсветки".into());
    }
    if brightness > 100 || !(1_000..=15_000).contains(&duration_ms) {
        return Err("Яркость — от 0 до 100%, длительность — от 1 до 15 секунд".into());
    }
    if color.len() != 7
        || !color.starts_with('#')
        || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err("Цвет должен быть в формате #RRGGBB".into());
    }
    let rgb: Vec<u8> = [1, 3, 5]
        .into_iter()
        .map(|offset| u8::from_str_radix(&color[offset..offset + 2], 16).unwrap())
        .collect();
    let count = duration_ms / 100;
    let mut frames = String::new();
    for tick in 0..count {
        for pixel in 0..24 {
            let (channels, level) = match animation {
                "rainbow" => (rainbow((pixel * 15 + tick * 9) % 360), 1.0),
                "pulse" => (
                    [rgb[0], rgb[1], rgb[2]],
                    0.15 + 0.85 * (1.0 - (tick as f64 * std::f64::consts::TAU / 20.0).cos()) / 2.0,
                ),
                _ => {
                    let distance = (tick + 24 - pixel) % 24;
                    (
                        [rgb[0], rgb[1], rgb[2]],
                        if distance < 8 {
                            (8 - distance) as f64 / 8.0
                        } else {
                            0.0
                        },
                    )
                }
            };
            // fbset on Cucumber: rgba 8/16,8/8,8/0,0/0, little endian.
            for channel in channels.into_iter().rev() {
                let value =
                    (f64::from(channel) * f64::from(brightness) / 100.0 * level).round() as u8;
                write!(frames, "\\{value:03o}").unwrap();
            }
        }
        frames.push('\n');
    }
    Ok(SCRIPT
        .replace("@FRAMES@", &frames)
        .replace("@COUNT@", &count.to_string())
        .replace("@DURATION_CS@", &(duration_ms / 10).to_string()))
}

fn rainbow(hue: u64) -> [u8; 3] {
    let rising = ((hue % 60) * 255 / 60) as u8;
    let falling = 255 - rising;
    match hue / 60 {
        0 => [255, rising, 0],
        1 => [falling, 255, 0],
        2 => [0, 255, rising],
        3 => [0, falling, 255],
        4 => [rising, 0, 255],
        _ => [255, 0, falling],
    }
}

const SCRIPT: &str = r#"fb=/dev/fb0
root=/sys/class/graphics/fb0
leds=/sys/class/leds
lock=/tmp/quasar-ring-overlay.lock
[ -c "$fb" ] && [ -r "$fb" ] && [ -w "$fb" ] || exit 2
for pair in name:ring virtual_size:24,1 bits_per_pixel:24 stride:72; do
 IFS= read -r value < "$root/${pair%%:*}" || exit 2
 [ "$value" = "${pair#*:}" ] || exit 2
done
format=$(fbset -fb "$fb" 2>/dev/null | awk '$1 == "rgba" {print $2}')
[ "$format" = 8/16,8/8,8/0,0/0 ] || exit 2
for i in 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23; do
 for color in red green blue; do
  IFS= read -r trigger < "$leds/cucumber:$color:ring$i/trigger" || exit 2
  case "$trigger" in *'[ledfb-ring]'*) : ;; *) exit 3 ;; esac
 done
done
native_idle() {
 for bank in ringA ringB; do
  IFS= read -r value < "$leds/cucumber:$bank:sync/brightness" || return 1
  [ "$value" = 0 ] || return 1
 done
}
native_idle || exit 3
uptime_cs() {
 read -r uptime unused < /proc/uptime || return 1
 fraction=${uptime#*.}
 [ "${#fraction}" -eq 2 ] || return 1
 now=$(( ${uptime%.*} * 100 + 1$fraction - 100 ))
}
uptime_cs || exit 2
sleep 0.001 2>/dev/null || exit 2
umask 077
mkdir "$lock" 2>/dev/null || exit 4
stage=''
owned=0
outcome=completed
read_frame() {
 dd if="$fb" of="$stage/current" bs=72 count=1 2>/dev/null || return 1
 [ "$(wc -c < "$stage/current")" -eq 72 ]
}
cleanup() {
 result=$?
 trap - EXIT HUP INT TERM
 if [ "$owned" -eq 1 ]; then
  if read_frame; then
   if cmp -s "$stage/current" "$stage/last" && native_idle; then
    cat "$stage/original" > "$fb" || result=7
   else
    outcome=interrupted
   fi
  else
   result=7
  fi
 fi
 if [ -n "$stage" ]; then rm -rf "$stage" || result=7; fi
 rmdir "$lock" 2>/dev/null || result=7
 if [ "$result" -eq 0 ]; then printf '%s' "$outcome"; fi
 exit "$result"
}
trap cleanup EXIT
trap 'outcome=interrupted; exit 0' HUP INT TERM
stage=$(mktemp -d /tmp/quasar-ring.XXXXXX) || exit 5
read_frame || exit 5
cp "$stage/current" "$stage/original" || exit 5
cp "$stage/current" "$stage/last" || exit 5
cat > "$stage/frames" <<'QUASAR_RING_FRAMES'
@FRAMES@QUASAR_RING_FRAMES
uptime_cs || exit 5
started=$now
step=0
while IFS= read -r frame; do
 uptime_cs || exit 5
 [ "$now" -lt "$((started + @DURATION_CS@))" ] || break
 read_frame || exit 5
 if ! cmp -s "$stage/current" "$stage/last" || ! native_idle; then
  outcome=interrupted
  exit 0
 fi
 printf '%b' "$frame" > "$stage/next" || exit 6
 [ "$(wc -c < "$stage/next")" -eq 72 ] || exit 6
 # Keep the expected frame before writing, including for a signal on write.
 cp "$stage/next" "$stage/last" || exit 6
 owned=1
 cat "$stage/next" > "$fb" || exit 6
 step=$((step + 1))
 uptime_cs || exit 5
 remaining=$((started + step * @DURATION_CS@ / @COUNT@ - now))
 if [ "$remaining" -gt 0 ]; then
  delay=$(printf '%d.%02d' "$((remaining / 100))" "$((remaining % 100))")
  sleep "$delay" || exit 6
 fi
done < "$stage/frames"
"#;
