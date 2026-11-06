use std::collections::BTreeSet;

use serde::Serialize;

#[derive(Clone, PartialEq, Serialize)]
pub struct DisplayDraft {
    pub mode: String,
    pub text: String,
    pub segments: BTreeSet<String>,
    pub animation: String,
    pub brightness: String,
    pub duration: String,
}

impl Default for DisplayDraft {
    fn default() -> Self {
        Self {
            mode: "text".into(),
            text: "HELLO".into(),
            segments: BTreeSet::new(),
            animation: "wave".into(),
            brightness: "60".into(),
            duration: "5".into(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize)]
pub struct LightDraft {
    pub animation: String,
    pub color: String,
    pub brightness: String,
    pub duration: String,
}

impl Default for LightDraft {
    fn default() -> Self {
        Self {
            animation: "wave".into(),
            color: "#a855f7".into(),
            brightness: "40".into(),
            duration: "5".into(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize)]
pub struct StationPreview {
    pub display: DisplayDraft,
    pub ring: LightDraft,
    pub logo: LightDraft,
}

#[derive(Clone, PartialEq, Serialize)]
pub(crate) struct DisplayFrame {
    pub segments: BTreeSet<String>,
    pub intensity: f64,
}

pub(crate) const DISPLAY_DIGIT_X: [i32; 4] = [12, 76, 158, 222];
pub(crate) const DISPLAY_SEGMENT_POLYGONS: [(&str, &str); 7] = [
    ("A", "7,0 35,0 41,6 35,12 7,12 1,6"),
    ("B", "36,8 42,14 42,31 36,37 30,31 30,14"),
    ("C", "36,39 42,45 42,62 36,68 30,62 30,45"),
    ("D", "7,64 35,64 41,70 35,76 7,76 1,70"),
    ("E", "6,39 12,45 12,62 6,68 0,62 0,45"),
    ("F", "6,8 12,14 12,31 6,37 0,31 0,14"),
    ("G", "7,32 35,32 41,38 35,44 7,44 1,38"),
];
pub(crate) const DISPLAY_EXTRA_POLYGONS: [(&str, &str); 3] = [
    ("PLUS1", "140,26 148,26 148,34 140,34"),
    ("PLUS2", "140,58 148,58 148,66 140,66"),
    (
        "DEGREE",
        "280,8 286,8 289,11 289,17 286,20 280,20 277,17 277,11",
    ),
];

pub(crate) fn display_frame(draft: &DisplayDraft, tick: usize) -> DisplayFrame {
    let segments = match draft.mode.as_str() {
        "text" => text_frame(&draft.text, tick),
        "frame" => draft.segments.clone(),
        _ => {
            let step = tick % 6;
            if draft.animation == "pulse" {
                text_frame("8888", 0)
            } else if draft.animation == "orbit" {
                (1..=4)
                    .map(|digit| format!("{}{}", ["A", "B", "C", "D", "E", "F"][step], digit))
                    .collect()
            } else {
                ('A'..='G')
                    .map(|segment| format!("{segment}{}", tick % 4 + 1))
                    .collect()
            }
        }
    };
    let brightness = draft
        .brightness
        .parse::<u8>()
        .ok()
        .filter(|value| *value <= 100)
        .unwrap_or(0);
    let intensity = if draft.mode == "animation" && draft.animation == "pulse" {
        [0.0, 0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.25, 0.0][tick % 9]
    } else {
        1.0
    } * f64::from(brightness)
        / 100.0;
    DisplayFrame {
        segments,
        intensity,
    }
}

fn glyph(character: char) -> &'static str {
    match character.to_ascii_uppercase() {
        '0' | 'O' => "ABCDEF",
        '1' | 'I' => "BC",
        '2' => "ABDEG",
        '3' => "ABCDG",
        '4' => "BCFG",
        '5' | 'S' => "ACDFG",
        '6' => "ACDEFG",
        'G' => "ACDEF",
        '7' => "ABC",
        '8' => "ABCDEFG",
        'B' => "CDEFG",
        '9' => "ABCDFG",
        'Q' => "ABCFG",
        'A' => "ABCEFG",
        'C' => "ADEF",
        'D' => "BCDEG",
        'E' => "ADEFG",
        'F' => "AEFG",
        'H' => "BCEFG",
        'J' => "BCDE",
        'L' => "DEF",
        'N' => "CEG",
        'P' => "ABEFG",
        'R' => "EG",
        'T' => "DEFG",
        'U' => "BCDEF",
        'Y' => "BCDFG",
        '-' => "G",
        '_' => "D",
        _ => "",
    }
}

fn text_frame(text: &str, tick: usize) -> BTreeSet<String> {
    let text = text.to_ascii_uppercase();
    let letters: Vec<char> = if text.len() > 4 {
        format!("    {text}    ").chars().collect()
    } else {
        format!("{}{text}", " ".repeat((4 - text.len()) / 2))
            .chars()
            .collect()
    };
    let offset = if letters.len() > 4 {
        tick % (letters.len() - 3)
    } else {
        0
    };
    letters
        .iter()
        .skip(offset)
        .take(4)
        .enumerate()
        .flat_map(|(index, letter)| {
            glyph(*letter)
                .chars()
                .map(move |segment| format!("{segment}{}", index + 1))
        })
        .collect()
}
