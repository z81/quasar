use dioxus::prelude::*;

#[component]
pub fn Icon(name: &'static str, #[props(default = 20)] size: u32) -> Element {
    let path = match name {
        "star" => "m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2L12 17.3 6.4 20.2 7.5 14 3 9.6l6.2-.9L12 3Z",
        "lock" => "M6 10h12v11H6z M8 10V6a4 4 0 0 1 8 0v4 M12 14v3",
        "key" => "M14 4a5 5 0 1 1-2 9L5 20H2v-3l7-7a5 5 0 0 1 5-6Z M16 7h.01",
        "settings" => "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Z M9 2h6l1 4 4 1 2 5-3 3 1 4-5 3-3-3-4 1-3-5 3-3-1-4 4-2 1-4",
        "logout" => "M9 3H4v18h5 M13 7l5 5-5 5 M8 12h13",
        "music" => "M9 18V5l12-2v13 M9 8l12-2 M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z M21 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z",
        "play" => "m8 4 12 8-12 8V4Z",
        "pause" => "M7 4h3v16H7z M14 4h3v16h-3z",
        "previous" => "M5 4v16 M19 4 7 12l12 8V4Z",
        "next" => "M19 4v16 M5 4l12 8-12 8V4Z",
        "grid" => "M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z",
        "history" => "M3 11a9 9 0 1 1 2.5 7 M3 4v7h7 M12 7v5l3 2",
        "code" => "m8 5-7 7 7 7 M16 5l7 7-7 7 M14 3l-4 18",
        "speaker" => "M8 3h8a3 3 0 0 1 3 3v12a3 3 0 0 1-3 3H8a3 3 0 0 1-3-3V6a3 3 0 0 1 3-3Z M12 6h.01 M15 14a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z",
        "arrow" => "M5 12h14 M13 6l6 6-6 6",
        "refresh" => "M20 7v5h-5 M4 17v-5h5 M6 6a8 8 0 0 1 13 2 M18 18A8 8 0 0 1 5 16",
        "sun" => "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Z M12 2v2 M12 20v2 M2 12h2 M20 12h2 M5 5l1.5 1.5 M17.5 17.5 19 19 M5 19l1.5-1.5 M17.5 6.5 19 5",
        "moon" => "M20.5 13.5A8.5 8.5 0 0 1 10.5 3 8.5 8.5 0 1 0 20.5 13.5Z",
        "volume-up" => "M11 4 5 9H2v6h3l6 5V4Z M15 8a6 6 0 0 1 0 8 M18 5a10 10 0 0 1 0 14",
        "volume-down" => "M11 4 5 9H2v6h3l6 5V4Z M15 8a6 6 0 0 1 0 8",
        "stop" => "M6 5h12a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Z",
        "spark" => "m12 2 2.8 7.2L22 12l-7.2 2.8L12 22l-2.8-7.2L2 12l7.2-2.8L12 2Z",
        "check" => "m5 12 4 4L19 6",
        "alert" => "M12 8v5 M12 17h.01 M10.3 3.8 2 18a1.3 1.3 0 0 0 1.2 2h17.6a1.3 1.3 0 0 0 1.2-2L13.7 3.8a2 2 0 0 0-3.4 0Z",
        "chevron" => "m9 5 7 7-7 7",
        "trash" => "M3 6h18 M9 6V3h6v3 M5 6l1 15h12l1-15 M10 10v7 M14 10v7",
        "search" => "M10.5 3a7.5 7.5 0 1 0 0 15 7.5 7.5 0 0 0 0-15Z m5.5 13 5 5",
        _ => "M12 3v18 M3 12h18",
    };
    rsx! {
        svg { width: size, height: size, view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.7", stroke_linecap: "round", stroke_linejoin: "round", "aria-hidden": "true",
            path { d: path }
        }
    }
}
