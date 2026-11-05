//! Bounded overlays for Midi's segmented clock, executed only inside an
//! owner-confirmed shell session. The layout and frame ownership rule follow
//! the documented Cucumber implementation in nyakokitsu/luna, gateway.rs.
//!
//! This is not a bitmap screen. Text is an approximation on four seven-segment
//! digits; pictures select physical segments. No arbitrary sysfs path, shell
//! fragment, or user text is interpolated into the generated script.

const SEGMENTS: [&str; 45] = [
    "A1", "B1", "C1", "D1", "E1", "F1", "G1", "A2", "B2", "C2", "D2", "E2", "F2", "G2", "A3", "B3",
    "C3", "D3", "E3", "F3", "G3", "A4", "B4", "C4", "D4", "E4", "F4", "G4", "PLUS1", "PLUS2",
    "DEGREE", "H1", "H2", "L1", "L2", "M1", "M2", "P1", "P2", "P4", "P5", "Q1", "Q2", "Q4", "Q5",
];

type Frame = [u8; 45];

pub(super) fn script(text: &str, brightness: u8, duration_ms: u64) -> Result<String, String> {
    if !text.is_ascii() || !(1..=32).contains(&text.len()) || text.trim().is_empty() {
        return Err("Надпись: от 1 до 32 символов латиницы или цифр, без пустой строки".into());
    }
    let glyphs = text.bytes().map(glyph).collect::<Result<Vec<_>, _>>()?;
    let mut padded = Vec::new();
    if glyphs.len() <= 4 {
        padded.resize((4 - glyphs.len()) / 2, 0);
        padded.extend(glyphs);
        padded.resize(4, 0);
    } else {
        padded.resize(4, 0);
        padded.extend(glyphs);
        padded.extend([0; 4]);
    }
    let frames: Vec<Frame> = padded
        .windows(4)
        .map(|digits| {
            let mut frame = [0; 45];
            for (digit, mask) in digits.iter().enumerate() {
                for segment in 0..7 {
                    if mask & (1 << segment) != 0 {
                        frame[digit * 7 + segment] = brightness;
                    }
                }
            }
            frame
        })
        .collect();
    overlay(&frames, brightness, duration_ms)
}

pub(super) fn frame_script(
    segments: &[String],
    brightness: u8,
    duration_ms: u64,
) -> Result<String, String> {
    if segments.len() > SEGMENTS.len() {
        return Err("В рисунке может быть не больше 45 сегментов".into());
    }
    let mut frame = [0; 45];
    for segment in segments {
        let Some(index) = SEGMENTS.iter().position(|known| known == segment) else {
            return Err("Неизвестный сегмент дисплея".into());
        };
        frame[index] = brightness;
    }
    overlay(&[frame], brightness, duration_ms)
}

pub(super) fn animation_script(
    animation: &str,
    brightness: u8,
    duration_ms: u64,
) -> Result<String, String> {
    let frames = match animation {
        "pulse" => [0_u16, 25, 50, 75, 100, 75, 50, 25, 0]
            .into_iter()
            .map(|level| {
                let mut frame = [0; 45];
                frame[..28].fill((u16::from(brightness) * level / 100) as u8);
                frame
            })
            .collect::<Vec<_>>(),
        "wave" => (0..4)
            .map(|digit| {
                let mut frame = [0; 45];
                frame[digit * 7..digit * 7 + 7].fill(brightness);
                frame
            })
            .collect(),
        "orbit" => (0..6)
            .map(|segment| {
                let mut frame = [0; 45];
                for digit in 0..4 {
                    frame[digit * 7 + segment] = brightness;
                }
                frame
            })
            .collect(),
        _ => return Err("Неизвестная сегментная анимация".into()),
    };
    overlay(&frames, brightness, duration_ms)
}

fn glyph(byte: u8) -> Result<u8, String> {
    // Bit order A B C D E F G: top, upper-right, lower-right, bottom,
    // lower-left, upper-left, middle. Ambiguous letters use lowercase shapes.
    Ok(match byte.to_ascii_uppercase() {
        b' ' => 0, b'-' => 0b1000000, b'_' => 0b0001000,
        b'0' | b'O' => 0b0111111, b'1' | b'I' => 0b0000110,
        b'2' => 0b1011011, b'3' => 0b1001111, b'4' => 0b1100110,
        b'5' | b'S' => 0b1101101, b'6' => 0b1111101, b'7' => 0b0000111,
        b'8' => 0b1111111, b'9' => 0b1101111, b'A' => 0b1110111,
        b'B' => 0b1111100, b'C' => 0b0111001, b'D' => 0b1011110,
        b'E' => 0b1111001, b'F' => 0b1110001, b'G' => 0b0111101,
        b'H' => 0b1110110, b'J' => 0b0011110, b'L' => 0b0111000,
        b'N' => 0b1010100, b'P' => 0b1110011, b'Q' => 0b1100111,
        b'R' => 0b1010000, b'T' => 0b1111000, b'U' => 0b0111110,
        b'Y' => 0b1101110,
        _ => return Err("Дисплей поддерживает цифры, ABCDEFGHIJLNOPQRSTUY, пробел, - и _. Другие буквы не помещаются в семь сегментов".into()),
    })
}

fn overlay(frames: &[Frame], brightness: u8, duration_ms: u64) -> Result<String, String> {
    if brightness > 100 {
        return Err("Яркость должна быть от 0 до 100".into());
    }
    if !(500..=15_000).contains(&duration_ms) {
        return Err("Длительность должна быть от 500 до 15000 мс".into());
    }
    let minimum = (frames.len() as u64 * 100).max(500);
    if duration_ms < minimum {
        return Err(format!(
            "Для этой надписи или анимации нужно не меньше {minimum} мс"
        ));
    }
    let names = SEGMENTS
        .iter()
        .map(|name| match *name {
            "PLUS1" => "p3".into(),
            "PLUS2" => "q3".into(),
            "DEGREE" => "deg".into(),
            other => other.to_ascii_lowercase(),
        })
        .collect::<Vec<String>>()
        .join(" ");
    let mut script = format!("names='{names}'\n");
    script.push_str(
        r#"root=/sys/class/leds
lock=/tmp/quasar-display-overlay.lock
for name in $names; do
 p="$root/cucumber:white:clock-$name/brightness"
 [ -f "$p" ] && [ -r "$p" ] && [ -w "$p" ] || exit 2
done
for p in "$root"/cucumber:sync:clock-*/brightness; do
 [ -e "$p" ] || continue
 IFS= read -r value < "$p" || exit 2
 [ "$value" = 0 ] || exit 3
done
sleep 0.001 2>/dev/null || exit 2
umask 077
mkdir "$lock" 2>/dev/null || exit 4
original=''
last=''
stop=0
outcome=completed
read_values() {
 for name in $names; do
  IFS= read -r value < "$root/cucumber:white:clock-$name/brightness" || return 1
  case "$value" in ''|*[!0-9]*) return 1;; esac
  [ "$value" -le 255 ] || return 1
  printf '%s ' "$value"
 done
}
restore_values() {
 set -- $original
 restore_failed=0
 for name in $names; do
  printf '%s\n' "$1" > "$root/cucumber:white:clock-$name/brightness" || restore_failed=1
  shift
 done
 return "$restore_failed"
}
cleanup() {
 result=$?
 trap - EXIT HUP INT TERM
 if [ -n "$last" ]; then
  current=$(read_values) || result=7
  if [ "$current" = "$last" ]; then
   restore_values || result=7
  else
   outcome=interrupted
  fi
 fi
 rmdir "$lock" 2>/dev/null || result=7
 if [ "$result" -eq 0 ]; then printf '%s' "$outcome"; fi
 exit "$result"
}
trap cleanup EXIT
trap 'stop=1; outcome=interrupted' HUP INT TERM
original=$(read_values) || exit 5
last=$original
put_frame() {
 [ "$stop" -eq 0 ] || exit 0
 current=$(read_values) || return 1
 if [ "$current" != "$last" ]; then outcome=interrupted; exit 0; fi
 # Update expected ownership after each successful write, so a write failure
 # or a signal mid-frame can still restore the partial frame we own.
 remaining=$last
 prefix=''
 for name in $names; do
  remaining=${remaining#* }
  printf '%s\n' "$1" > "$root/cucumber:white:clock-$name/brightness" || return 1
  prefix="$prefix$1 "
  last="$prefix$remaining"
  shift
 done
}
"#,
    );
    let count = frames.len() as u64;
    for (index, frame) in frames.iter().enumerate() {
        let values = frame
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let delay = duration_ms * (index as u64 + 1) / count - duration_ms * index as u64 / count;
        script.push_str(&format!("put_frame {values} || exit 6\n[ \"$stop\" -eq 0 ] || exit 0\nsleep {}.{:03} || [ \"$stop\" -ne 0 ] || exit 6\n", delay / 1000, delay % 1000));
    }
    Ok(script)
}
