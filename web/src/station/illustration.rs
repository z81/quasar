use dioxus::prelude::*;

const SHELL: &str = "M43 37 105 55Q114 59 128 56L261 38V191Q261 214 244 224L129 244Q114 247 103 242L63 225Q43 217 43 195Z";
const LID: &str = "M44 24Q47 22 57 21L164 8Q181 6 196 11L254 29Q266 33 260 38L128 57Q115 60 104 56L48 35Q39 31 44 24Z";

// Decorative dot-matrix clock, matching the supplied product reference.
const CLOCK: [&str; 7] = [
    "00100 01110 0 11110 11111",
    "01100 10001 0 00001 10000",
    "00100 00001 1 00001 10000",
    "00100 00010 0 01110 11110",
    "00100 00100 1 00001 00001",
    "00100 01000 0 00001 00001",
    "01110 11111 0 11110 11110",
];

#[component]
pub(super) fn Midi() -> Element {
    rsx! {
        svg { class: "station-midi", view_box: "20 0 260 270", fill: "none", "aria-hidden": "true", "focusable": "false",
            defs {
                linearGradient { id: "midi-shell", x1: "43", y1: "120", x2: "261", y2: "140", gradient_units: "userSpaceOnUse",
                    stop { offset: "0", stop_color: "#414349" }
                    stop { offset: ".24", stop_color: "#2a2c31" }
                    stop { offset: ".4", stop_color: "#17191d" }
                    stop { offset: ".72", stop_color: "#202227" }
                    stop { offset: "1", stop_color: "#2d3036" }
                }
                linearGradient { id: "midi-lid", x1: "105", y1: "8", x2: "165", y2: "62", gradient_units: "userSpaceOnUse",
                    stop { stop_color: "#414248" }
                    stop { offset: "1", stop_color: "#15161a" }
                }
                linearGradient { id: "midi-edge", x1: "43", y1: "35", x2: "261", y2: "240", gradient_units: "userSpaceOnUse",
                    stop { stop_color: "#a5a8b5", stop_opacity: ".3" }
                    stop { offset: ".55", stop_color: "#9296a4", stop_opacity: ".03" }
                    stop { offset: "1", stop_color: "#a5a8b5", stop_opacity: ".18" }
                }
                pattern { id: "midi-weave", width: "3", height: "4", pattern_units: "userSpaceOnUse",
                    path { d: "m0 1 1 2m1.5-3 1 2", stroke: "#a0a4b0", stroke_width: ".65", stroke_opacity: ".24" }
                    path { d: "m0 3 1 1m1.5-2 1 1", stroke: "#090b0e", stroke_width: ".8", stroke_opacity: ".65" }
                }
                clipPath { id: "midi-silhouette", path { d: SHELL } }
                filter { id: "midi-glow", x: "-20%", y: "-100%", width: "140%", height: "300%",
                    feGaussianBlur { std_deviation: "2" }
                }
                filter { id: "midi-shadow", x: "-30%", y: "-100%", width: "160%", height: "300%",
                    feGaussianBlur { std_deviation: "7" }
                }
            }
            ellipse { class: "midi-shadow", cx: "153", cy: "247", rx: "98", ry: "10", fill: "#000", filter: "url(#midi-shadow)" }
            path { d: SHELL, fill: "url(#midi-shell)", stroke: "url(#midi-edge)", stroke_width: "1.2" }
            g { clip_path: "url(#midi-silhouette)",
                path { d: SHELL, fill: "url(#midi-weave)" }
                path { d: "M105 57Q101 151 107 239", stroke: "#70747e", stroke_width: "1", stroke_opacity: ".1" }
            }
            path { d: LID, transform: "translate(0 3)", fill: "#0e1014" }
            g { class: "midi-light",
                path { d: "M45 37 105 58Q115 62 128 59L258 41", stroke: "#ad5bff", stroke_width: "4", filter: "url(#midi-glow)", opacity: ".65" }
                path { d: "M45 37 105 58Q115 62 128 59L258 41", stroke: "#b879ff", stroke_width: "1.8", stroke_linecap: "round" }
            }
            path { d: LID, fill: "url(#midi-lid)", stroke: "#737581", stroke_width: ".65", stroke_opacity: ".4" }
            g { stroke: "#777984", stroke_width: "1.2", stroke_linecap: "round", opacity: ".65",
                path { d: "m107 35 6-.8m73-9 5-.6m-2.5-1.5v3.6" }
                circle { cx: "163", cy: "17", r: "1", fill: "#777984", stroke: "none" }
            }
            ellipse { cx: "148", cy: "31", rx: "8", ry: "2.3", fill: "#d6cfe3" }
            ellipse { class: "midi-light", cx: "148", cy: "31", rx: "5", ry: "1.4", fill: "#a856ed" }
            g { class: "midi-clock", transform: "translate(153 111) skewY(-8)", fill: "#bfd4f0",
                for (y, row) in CLOCK.iter().enumerate() {
                    for (x, pixel) in row.bytes().enumerate() {
                        if pixel == b'1' { circle { cx: (x as f32 * 3.2).to_string(), cy: (y as f32 * 3.5).to_string(), r: "1.15" } }
                    }
                }
            }
        }
    }
}
