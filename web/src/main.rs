mod access;
mod animation_timeline;
mod api;
mod automations;
mod composer;
mod device_access;
mod device_tools;
mod display_settings;
mod display_studio;
mod dnd_settings;
mod favorites;
mod history;
mod home_assistant;
mod icons;
mod maintenance;
mod midi_display;
mod overview;
mod packages;
mod player;
mod ring_light;
mod schedules;
mod server_settings;
mod settings;
mod sidebar;
mod smart_home;
mod sound_settings;
mod speaker_profiles;
mod state;
mod station;
mod station_preview;
mod system_sounds;
mod timers;
mod weather;
mod whisper_settings;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use icons::Icon;
use state::{ControlState, Panel, Theme};

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut state = ControlState {
        devices: use_signal(Vec::new),
        selected: use_signal(String::new),
        loading: use_signal(|| true),
        server_error: use_signal(|| None),
        sending: use_signal(|| false),
        history: use_signal(Vec::new),
        history_error: use_signal(|| None),
        favorites: use_signal(Vec::new),
        persistent: use_signal(|| None),
        player: use_signal(|| None),
        player_error: use_signal(|| None),
        weather: use_signal(|| None),
        weather_error: use_signal(|| None),
        weather_loading: use_signal(|| false),
        weather_request: use_signal(|| 0),
        station_previews: use_signal(Default::default),
        auth: use_signal(Default::default),
        auth_loaded: use_signal(|| false),
        auth_error: use_signal(|| None),
        mode: use_signal(Default::default),
        text: use_signal(String::new),
        panel: use_signal(Default::default),
        theme: use_signal(Theme::load),
        notice: use_signal(|| None),
    };
    use_future(move || async move {
        loop {
            api::refresh_auth(state).await;
            if state.allowed() {
                api::refresh(state).await;
                api::refresh_saved(state).await;
            }
            TimeoutFuture::new(10_000).await;
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                api::refresh_history(state).await;
            }
            let active = state
                .history
                .read()
                .iter()
                .any(|entry| matches!(entry.status.as_str(), "queued" | "running"));
            TimeoutFuture::new(if active { 2_000 } else { 10_000 }).await;
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                api::refresh_player(state).await;
            }
            TimeoutFuture::new(5_000).await;
        }
    });
    use_effect(move || {
        let _ = (state.selected)();
        state.player.set(None);
        if state.allowed() {
            spawn(api::refresh_player(state));
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                weather::refresh(state).await;
            }
            TimeoutFuture::new(60_000).await;
        }
    });
    use_effect(move || {
        let _ = (state.selected)();
        state.weather.set(None);
        state.weather_error.set(None);
        if state.allowed() {
            spawn(weather::refresh(state));
        }
    });
    use_effect(move || {
        let _ = (state.panel)();
        if let Some(window) = web_sys::window() {
            window.scroll_to_with_x_and_y(0.0, 0.0);
        }
    });
    let panel = (state.panel)();
    let font_faces = [
        (400, asset!("/assets/ocean/manrope-400.woff2")),
        (500, asset!("/assets/ocean/manrope-500.woff2")),
        (600, asset!("/assets/ocean/manrope-600.woff2")),
        (700, asset!("/assets/ocean/manrope-700.woff2")),
        (800, asset!("/assets/ocean/manrope-800.woff2")),
    ].into_iter().map(|(weight, url)| format!("@font-face {{ font-family: Manrope; src: url('{url}') format('woff2'); font-weight: {weight}; font-display: swap; }}")).collect::<String>();
    let background = asset!("/assets/ocean/background.webp");
    rsx! {
        document::Title { "Quasar — пульт управления" }
        document::Link { rel: "manifest", href: "/ui/app.webmanifest" }
        document::Script { r#type: "module", src: asset!("/assets/pwa.js") }
        document::Stylesheet { href: asset!("/assets/control.css") }
        document::Stylesheet { href: asset!("/assets/ocean.css") }
        document::Stylesheet { href: asset!("/assets/ocean-sections.css") }
        document::Style { "{font_faces}" }
        div { class: if state.allowed() { "app" } else { "app auth-app" }, "data-theme": (state.theme)().name(), style: "--ocean-background: url('{background}');",
            if !state.allowed() { access::LoginGate { state } } else {
            a { class: "skip-link", href: "#main-content", "К содержимому" }
            sidebar::Sidebar { state }
            div { class: "workspace",
                main { class: "main", id: "main-content", tabindex: "-1",
                    sidebar::SectionNavigation { state }
                    if panel != Panel::Control { div { class: "page-heading",
                        div {
                            h1 { "{panel.label()}" }
                            p { match panel {
                                Panel::Control => "Музыка, громкость и команды выбранной колонке.",
                                Panel::Music => "Воспроизведение, поиск и ваша музыка.",
                                Panel::Home => "Устройства, комнаты и управление умным домом.",
                                Panel::History => "Запросы, ответы Алисы и результаты обработки.",
                                Panel::Advanced => "Отправляйте колонке команды её собственного протокола.",
                                Panel::Favorites => "Любимые команды для каждого дня. Сохраните и запускайте одним нажатием.",
                                Panel::Schedules => "Планы на утро, важные дела и время для себя.",
                                Panel::Timers => "Для чая, готовки и короткой паузы. Колонка напомнит сигналом.",
                                Panel::Settings => "Модели, речь и интеграции сервера. Имя и голосовые настройки колонок.",
                                Panel::StationSettings => "Голос, шёпот, расписание тишины и звук колонки.",
                                Panel::Access => "Вход в пульт и ключи для ваших интеграций.",
                                Panel::DeviceAccess => "Терминал и файлы выбранной колонки через ADB.",
                                Panel::MidiDisplay => "Свои надписи, рисунки и анимации дисплея и верхней подсветки Миди.",
                                Panel::SmartHome => "Устройства Zigbee и Matter, подключённые к выбранной колонке.",
                                Panel::Automations => "Сценарии на сервере и автономные будильники на колонке.",
                                Panel::DeviceTools => "Состояние системы, история показателей, журналы и Bluetooth.",
                                Panel::Maintenance => "Пакеты, службы, системные звуки и резервные копии.",
                            } }
                        }
                    } }
                    if let Some(error) = (state.server_error)() {
                        div { class: "notice error", role: "alert", Icon { name: "alert" } span { "{error}. Повторяем подключение автоматически." } }
                    }
                    if let Some(notice) = (state.notice)() {
                        div { class: "notice error", role: "alert", Icon { name: "alert" } span { "{notice}" }
                            button { class: "dismiss", aria_label: "Закрыть уведомление", onclick: move |_| state.notice.set(None), "×" }
                        }
                    }
                    match panel {
                        Panel::Control => rsx! {
                            div { class: "ocean-dashboard",
                                div { class: "ocean-left-column",
                                    composer::Composer { state }
                                    player::PlayerCard { state }
                                    overview::Today { state }
                                }
                                div { class: "ocean-right-column",
                                    overview::StationOverview { state }
                                    weather::WeatherCard { state }
                                }
                                div { class: "ocean-recent", history::History { state, compact: true } }
                            }
                        },
                        Panel::Music => rsx! { player::MusicPage { state } },
                        Panel::Home => rsx! { home_assistant::HomeAssistant { state } },
                        Panel::History => rsx! { history::History { state, compact: false } },
                        Panel::Favorites => rsx! { favorites::Favorites { state } },
                        Panel::Schedules => rsx! { schedules::Schedules { state } },
                        Panel::Timers => rsx! { timers::Timers { state } },
                        Panel::Settings => rsx! { settings::Settings { key: "{panel.label()}", state } },
                        Panel::StationSettings => rsx! { settings::Settings { key: "{panel.label()}", state, station: true } },
                        Panel::Access => rsx! { access::Access { state } },
                        Panel::DeviceAccess => rsx! { device_access::DeviceAccessPage { state } },
                        Panel::MidiDisplay => rsx! { device_access::DeviceAccessPage { state, mode: device_access::AccessMode::Display } },
                        Panel::SmartHome => rsx! { smart_home::SmartHomePage { state } },
                        Panel::Automations => rsx! { automations::AutomationsPage { state } },
                        Panel::DeviceTools => rsx! { device_tools::DeviceToolsPage { state } },
                        Panel::Maintenance => rsx! { maintenance::MaintenancePage { state } },
                        Panel::Advanced => rsx! {
                            div { class: "control-grid", composer::Advanced { state } station::StationCard { state } }
                        },
                    }
                }
            }
            }
        }
    }
}
