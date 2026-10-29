//! Per-station quasar ("maind") config: a port of the zod schema in TS
//! `storage/types.ts` with its deepDefault defaults, the parsing zod does on
//! every read and on API updates (defaults filled in, unknown keys stripped,
//! zod-style issues), and the `get_sync_info` / `glagol/device_list`
//! projections of `routers/quasar.yandex.net.ts`.

use std::sync::LazyLock;

use serde_json::{Map, Value, json};

static QUASAR_CONFIG: LazyLock<Schema> = LazyLock::new(quasar_config_schema);

/// The config of a new station (TS getDefaultQuasarConfig)
pub fn default_config() -> Value {
    QUASAR_CONFIG.default.clone().unwrap_or_default()
}

/// zod `quasarConfig.parse`: the config with defaults filled in and unknown
/// keys stripped, plus zod-style issues whose paths start with `path`. An
/// invalid value is replaced by the nearest default, so lenient callers can
/// still use the config; strict callers reject it when there are issues.
pub fn parse(input: Option<&Value>, path: &[&str]) -> (Value, Vec<Value>) {
    let mut path: Vec<Value> = path.iter().map(|segment| Value::from(*segment)).collect();
    let mut issues = Vec::new();
    let config = match QUASAR_CONFIG.parse(input, &mut path, &mut issues) {
        Parsed::Value(config) => config,
        Parsed::Absent | Parsed::Invalid => default_config(),
    };
    (config, issues)
}

/// A stored config, re-parsed so that new defaults apply to old rows (TS
/// toInfo / initialize). TS fails on an invalid stored config; here the
/// invalid fields fall back to their defaults and the problem is logged.
pub fn from_stored(duid: &str, stored: Option<&Value>) -> Value {
    let (config, issues) = parse(stored.filter(|stored| !stored.is_null()), &[]);
    if !issues.is_empty() {
        let issues = Value::Array(issues).to_string();
        tracing::error!("stored quasar config of {duid} is invalid, using defaults for the invalid fields: {issues}");
    }
    config
}

/// The zod `invalid_type` issue
pub fn invalid_type_issue(expected: &str, input: Option<&Value>, path: &[Value]) -> Value {
    let received = match input {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    };
    json!({
        "expected": expected,
        "code": "invalid_type",
        "path": path,
        "message": format!("Invalid input: expected {expected}, received {received}"),
    })
}

/// TS mapStationInfoToGlagolDeviceConfig: the `config` of a glagol/device_list entry
pub fn glagol_device_config(config: &Value, name: &str) -> Value {
    let device = &config["deviceConfig"];
    let led = &device["led"];
    let mut result = json!({
        "dndMode": device["dndMode"],
        "led": {
            "brightness": led["brightness"],
            "idle_animation": led["idleAnimation"],
            "music_equalizer_visualization": led["musicEqualizerVisualization"],
            "time_visualization": led["timeVisualization"],
        },
        "locale": device["locale"],
        "location": device.get("location").cloned().unwrap_or_else(|| json!({})),
        "name": name,
        "standby": device["standby"],
    });
    if let Some(stereo_pair) = device.get("stereoPair") {
        result["stereo_pair"] = stereo_pair.clone();
    }
    result
}

/// The get_sync_info response body (TS routers/quasar.yandex.net.ts)
pub fn sync_info(config: &Value, name: &str, certificate: &str, private_key: &str) -> Value {
    let subscription = &config["aliceProSubscription"];
    let enabled = subscription["enabled"].as_bool().unwrap_or_default();
    let mut alice_pro_subscription = json!({ "enabled": enabled });
    if enabled {
        alice_pro_subscription["ttl"] = subscription["ttl"].clone();
    }
    json!({
        "alice_pro_subscription": alice_pro_subscription,
        "config": sync_info_config(config, name),
        "glagol": {
            "security": {
                "server_certificate": certificate,
                "server_private_key": private_key,
            },
        },
        "status": "ok",
        "subscription": { "mode": "transaction" },
        "tags": [],
    })
}

/// TS mapStationInfoToStationSyncInfoResponseConfig. The config is parsed, so
/// its objects hold exactly the schema keys: sections the station reads under
/// the same key names are copied whole, renamed ones are mapped field by field.
fn sync_info_config(config: &Value, name: &str) -> Value {
    let account = &config["accountConfig"];
    let device = &config["deviceConfig"];
    let system = &config["systemConfig"];

    let analytics = &system["analyticsEnvironment"];
    let environment = json!({
        "quasmodrom_group": analytics["quasmodromGroup"],
        "quasmodrom_subgroup": analytics["quasmodromSubgroup"],
        "test_buckets": analytics["testBuckets"],
        "testids": analytics["testids"],
    });

    let account_config = json!({
        "aliceAdaptiveVolume": account["aliceAdaptiveVolume"],
        "aliceProactivity": account["aliceProactivity"],
        "alwaysOnMicForShortcuts": account["alwaysOnMicForShortcuts"],
        "audio_player": account["audioPlayer"],
        "childContentAccess": account["childContentAccess"],
        "contentAccess": account["contentAccess"],
        "doNotUseUserLogs": account["doNotUseUserLogs"],
        "enableChildVad": account["enableChildVad"],
        "enabledCommandSpotters": account["enabledCommandSpotters"],
        "jingle": account["jingle"],
        "saveHistoryUsage": account["saveHistoryUsage"],
        "smartActivation": account["smartActivation"],
        "spotter": account["spotter"],
        "useBiometryChildScoring": account["useBiometryChildScoring"],
        "user_wifi_config": { "wifi_hash": account["userWifiConfig"]["wifiHash"] },
        "useRichModelForPro": account["useRichModelForPro"],
    });

    let mut device_config = glagol_device_config(config, name);
    device_config["beta"] = device["beta"].clone();
    device_config["tv_beta"] = device["tvBeta"].clone();

    let scoring = &system["bioCapability"]["engineConfig"]["contextScoring"];
    let calld = &system["calld"];
    let pre_amplifier = &calld["audioProcessingConfig"]["preAmplifier"];
    let hybrid = &system["hybridRequestFactory"];
    let phone_calls = &system["phoneCalls"];
    let groups: Vec<Value> = system["telemetry"]["rateLimiter"]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| json!({
            "count_limit": group["countLimit"],
            "events": group["events"],
            "interval_ms": group["intervalMs"],
        }))
        .collect();
    json!({
        "account_config": account_config,
        "audio_player_capability": system["audioPlayerCapability"],
        "audioclient": system["audioclient"],
        "bio_capability": {
            "engine_config": {
                "context_scoring": {
                    "enabled": scoring["enabled"],
                    "guest_threshold": scoring["guestThreshold"],
                    "lambda": scoring["lambda"],
                    "max_time_diff_seconds": scoring["maxTimeDiffSeconds"],
                    "min_embedding_cosine": scoring["minEmbeddingCosine"],
                    "use_exp_formula": scoring["useExpFormula"],
                },
            },
            "use_ondevice_classification": system["bioCapability"]["useOnDeviceClassification"],
        },
        "calld": {
            "audio_processing_config": {
                "pre_amplifier": {
                    "enabled": pre_amplifier["enabled"],
                    "fixed_gain_factor": pre_amplifier["fixedGainFactor"],
                },
            },
            "auto_gain_control": calld["autoGainControl"],
            "tx_agc_digital_compression_gain": calld["txAgcDigitalCompressionGain"],
        },
        "com.yandex.capabilities": system["comYandexCapabilities"],
        "com.yandex.tv.home": system["comYandexTvHome"],
        "com.yandex.tv.services": system["comYandexTvServices"],
        "device_config": device_config,
        "device_control_panel": system["deviceControlPanel"],
        "env": environment,
        "experiments": system["experiments"],
        "fluent-bit": system["fluentBit"],
        "forceWifiReconfigure": system["forceWifiReconfigure"],
        "hybrid_request_factory": {
            "hybrid": {
                "scenarios": {
                    "fast_command_scenario": {
                        "supported_frames": hybrid["hybrid"]["scenarios"]["fastCommandScenario"]["supportedFrames"],
                    },
                },
            },
            "hybrid_text_inputs": hybrid["hybridTextInputs"],
            "use_protobuf_analytics_info": hybrid["useProtobufAnalyticsInfo"],
        },
        "iot": system["iot"],
        "maind": system["maind"],
        "matter": system["matter"],
        "mediad": system["mediad"],
        "onlineSpotterEnabled": system["onlineSpotterEnabled"],
        "phone_calls": {
            "block_updates_on_call": phone_calls["blockUpdatesOnCall"],
            "enabled": phone_calls["enabled"],
            "webrtc": phone_calls["webrtc"],
        },
        "qrPaymentConfig": system["qrPaymentConfig"],
        "quasmodrom_group": analytics["quasmodromGroup"],
        "quasmodrom_subgroup": analytics["quasmodromSubgroup"],
        "sendVinsRequestProto": system["sendVinsRequestProto"],
        "shouldCheckWifi": system["shouldCheckWifi"],
        "smart_volume": system["smartVolume"],
        "system_config": {
            "adbd": system["adbd"],
            "addAllEmbeddedEndpoints": system["addAllEmbeddedEndpoints"],
            "appmetrikaReportEnvironment": environment,
            "audioInput": {
                "model_storage": {
                    "models": sync_info_models(&system["audioInput"]["modelStorage"]["models"]),
                },
            },
        },
        "teleme3d": {
            "appmetrica": { "+events": system["teleme3d"]["appmetrica"]["plusEvents"] },
        },
        "telemetry": { "rate_limiter": { "groups": groups } },
        "unbound": system["unbound"],
        "useVinsRequestProto": system["useVinsRequestProto"],
        "voice_activity_detector": {
            "events_enabled": system["voiceActivityDetector"]["eventsEnabled"],
        },
        "voiceDialogSettings": system["voiceDialogSettings"],
        "wifi_capability": { "enabled_v2": system["wifiCapability"]["enabledV2"] },
        "wifiSettings": system["wifiSettings"],
        "zigbee": system["zigbee"],
    })
}

/// TS passes crc, fallbackUrls, format, url and word (when set) of each model
fn sync_info_models(models: &Value) -> Value {
    let mut result = Map::new();
    for (group, entries) in models.as_object().into_iter().flatten() {
        let mut mapped = Map::new();
        for (key, model) in entries.as_object().into_iter().flatten() {
            let mut entry = json!({
                "crc": model["crc"],
                "fallbackUrls": model["fallbackUrls"],
                "format": model["format"],
                "url": model["url"],
            });
            if let Some(word) = model.get("word") {
                entry["word"] = word.clone();
            }
            mapped.insert(key.clone(), entry);
        }
        result.insert(group.clone(), Value::Object(mapped));
    }
    Value::Object(result)
}

/// zod schema node
enum Node {
    Boolean,
    Number,
    String,
    Literal(Value),
    /// z.union: the first option without issues wins; one option acts as itself
    Union(Vec<Schema>),
    /// z.object: declared keys in order, unknown keys stripped
    Object(Vec<(&'static str, Schema)>),
    Array(Box<Schema>),
    /// z.record(z.string(), value)
    Record(Box<Schema>),
}

struct Schema {
    node: Node,
    /// `.default(...)`, or the computed default of an object whose fields all
    /// have defaults or are optional (TS deepDefault)
    default: Option<Value>,
    optional: bool,
}

enum Parsed {
    Value(Value),
    /// an optional value that is not set
    Absent,
    /// no value and no default: the nearest ancestor with a default replaces it
    Invalid,
}

impl Schema {
    fn new(node: Node) -> Self {
        Self { node, default: None, optional: false }
    }

    fn default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    fn parse(&self, input: Option<&Value>, path: &mut Vec<Value>, issues: &mut Vec<Value>) -> Parsed {
        if input.is_none() {
            if let Some(default) = &self.default {
                return Parsed::Value(default.clone());
            }
            if self.optional {
                return Parsed::Absent;
            }
        }
        match self.node.parse(input, path, issues) {
            Some(value) => Parsed::Value(value),
            None => match &self.default {
                Some(default) => Parsed::Value(default.clone()),
                None if self.optional => Parsed::Absent,
                None => Parsed::Invalid,
            },
        }
    }

}

impl Node {
    fn parse(&self, input: Option<&Value>, path: &mut Vec<Value>, issues: &mut Vec<Value>) -> Option<Value> {
        match self {
            Self::Boolean => match input {
                Some(value @ Value::Bool(_)) => Some(value.clone()),
                _ => invalid_type("boolean", input, path, issues),
            },
            Self::Number => match input {
                Some(Value::Number(number)) => Some(js_number(number)),
                _ => invalid_type("number", input, path, issues),
            },
            Self::String => match input {
                Some(value @ Value::String(_)) => Some(value.clone()),
                _ => invalid_type("string", input, path, issues),
            },
            Self::Literal(expected) => {
                if input == Some(expected) {
                    return Some(expected.clone());
                }
                issues.push(json!({
                    "code": "invalid_value",
                    "values": [expected],
                    "path": path,
                    "message": format!("Invalid input: expected {expected}"),
                }));
                None
            }
            Self::Union(options) if options.len() == 1 => match options[0].parse(input, path, issues) {
                Parsed::Value(value) => Some(value),
                Parsed::Absent | Parsed::Invalid => None,
            },
            Self::Union(options) => {
                let mut errors = Vec::new();
                for option in options {
                    // option issues have paths relative to the union
                    let mut option_issues = Vec::new();
                    if let Parsed::Value(value) = option.parse(input, &mut Vec::new(), &mut option_issues) {
                        if option_issues.is_empty() {
                            return Some(value);
                        }
                    }
                    errors.push(Value::Array(option_issues));
                }
                issues.push(json!({
                    "code": "invalid_union",
                    "errors": errors,
                    "path": path,
                    "message": "Invalid input",
                }));
                None
            }
            Self::Object(fields) => {
                let Some(Value::Object(object)) = input else {
                    return invalid_type("object", input, path, issues);
                };
                let mut result = Map::new();
                let mut valid = true;
                for (key, field) in fields {
                    path.push(Value::from(*key));
                    match field.parse(object.get(*key), path, issues) {
                        Parsed::Value(value) => {
                            result.insert((*key).to_string(), value);
                        }
                        Parsed::Absent => {}
                        Parsed::Invalid => valid = false,
                    }
                    path.pop();
                }
                valid.then_some(Value::Object(result))
            }
            Self::Array(item) => {
                let Some(Value::Array(items)) = input else {
                    return invalid_type("array", input, path, issues);
                };
                let mut result = Vec::with_capacity(items.len());
                for (index, value) in items.iter().enumerate() {
                    path.push(Value::from(index));
                    if let Parsed::Value(value) = item.parse(Some(value), path, issues) {
                        result.push(value);
                    }
                    path.pop();
                }
                (result.len() == items.len()).then_some(Value::Array(result))
            }
            Self::Record(value_schema) => {
                let Some(Value::Object(object)) = input else {
                    return invalid_type("record", input, path, issues);
                };
                let mut result = Map::new();
                for (key, value) in object {
                    path.push(Value::from(key.as_str()));
                    if let Parsed::Value(value) = value_schema.parse(Some(value), path, issues) {
                        result.insert(key.clone(), value);
                    }
                    path.pop();
                }
                (result.len() == object.len()).then_some(Value::Object(result))
            }
        }
    }
}

fn invalid_type(expected: &str, input: Option<&Value>, path: &[Value], issues: &mut Vec<Value>) -> Option<Value> {
    issues.push(invalid_type_issue(expected, input, path));
    None
}

/// JS has one number type: 240.0 is stored and sent as 240
fn js_number(number: &serde_json::Number) -> Value {
    match number.as_f64() {
        Some(float)
            if !number.is_i64()
                && !number.is_u64()
                && float.fract() == 0.0
                && float.abs() < 9_007_199_254_740_992.0 =>
        {
            Value::from(float as i64)
        }
        _ => Value::Number(number.clone()),
    }
}

fn boolean(default: bool) -> Schema {
    Schema::new(Node::Boolean).default(default)
}

fn number(default: impl Into<Value>) -> Schema {
    Schema::new(Node::Number).default(default)
}

fn string(default: &str) -> Schema {
    Schema::new(Node::String).default(default)
}

fn strings(default: &[&str]) -> Schema {
    Schema::new(Node::Array(Box::new(Schema::new(Node::String)))).default(default.to_vec())
}

fn literal(value: impl Into<Value>) -> Schema {
    Schema::new(Node::Literal(value.into()))
}

fn one_of(literals: &[&str]) -> Schema {
    Schema::new(Node::Union(literals.iter().map(|value| literal(*value)).collect()))
}

/// z.object, with TS deepDefault's computed default
fn object(fields: Vec<(&'static str, Schema)>) -> Schema {
    let default = fields
        .iter()
        .all(|(_, field)| field.default.is_some() || field.optional)
        .then(|| {
            Value::Object(
                fields
                    .iter()
                    .filter_map(|(key, field)| field.default.clone().map(|default| ((*key).to_string(), default)))
                    .collect(),
            )
        });
    Schema { node: Node::Object(fields), default, optional: false }
}

/// TS storage/types.ts rawQuasarConfig, field by field
fn quasar_config_schema() -> Schema {
    let account_config = object(vec![
        ("aliceAdaptiveVolume", object(vec![("enabled", boolean(true))])),
        ("aliceProactivity", boolean(true)),
        ("alwaysOnMicForShortcuts", boolean(false)),
        ("audioPlayer", object(vec![("crossfadeEnabled", boolean(true))])),
        ("childContentAccess", one_of(&["children", "without", "safe"]).default("without")),
        ("contentAccess", one_of(&["children", "medium", "without", "safe"]).default("without")),
        ("doNotUseUserLogs", boolean(false)),
        ("enableChildVad", boolean(false)),
        ("enabledCommandSpotters", object(vec![
            ("call", object(vec![("answer", boolean(false))])),
            ("music", object(vec![
                ("bluetooth", boolean(false)),
                ("feedback", boolean(false)),
                ("navigation", boolean(false)),
                ("playAndPause", boolean(false)),
                ("volume", boolean(false)),
            ])),
            ("smartHome", object(vec![("light", boolean(false)), ("tv", boolean(false))])),
            ("tv", object(vec![("backToHome", boolean(false)), ("navigation", boolean(false))])),
        ])),
        // the "blimp" sound on activation and cancellation
        ("jingle", boolean(false)),
        ("saveHistoryUsage", boolean(true)),
        ("smartActivation", boolean(true)),
        // the spotter word; working ones: alisa, yandex and yasmina
        ("spotter", string("alisa")),
        ("useBiometryChildScoring", boolean(true)),
        ("useRichModelForPro", boolean(true)),
        ("userWifiConfig", object(vec![(
            "wifiHash",
            string("39671801dd40273b54f83b5c3f7d9a0f0055e5a1b1687a6427241d9be9bf402e"),
        )])),
    ]);

    let alice_pro_subscription = Schema::new(Node::Union(vec![
        object(vec![("enabled", literal(true)), ("ttl", Schema::new(Node::Number))]),
        object(vec![("enabled", literal(false))]),
    ]))
    .default(json!({ "enabled": true, "ttl": 365 * 24 * 60 * 60 }));

    let device_config = object(vec![
        ("beta", boolean(false)),
        ("dndMode", object(vec![
            ("enabled", boolean(false)),
            ("features", object(vec![("allowIncomingCalls", boolean(false))])),
        ])),
        ("led", object(vec![
            ("brightness", object(vec![("auto", boolean(true)), ("value", number(0.5))])),
            // an idle animation after 20 seconds of idling
            ("idleAnimation", boolean(false)),
            ("musicEqualizerVisualization", object(vec![
                // switch LED patterns by themselves while music plays
                ("auto", boolean(false)),
                // the pattern while music plays (auto: false): lava_beat, blink, polar_shining or none
                ("style", string("lava_beat")),
            ])),
            ("timeVisualization", object(vec![("format", one_of(&["24h", "12h"]).default("24h"))])),
        ])),
        ("locale", string("ru-RU")),
        ("location", object(vec![
            ("latitude", Schema::new(Node::Number)),
            ("longitude", Schema::new(Node::Number)),
        ])
        .optional()),
        ("standby", object(vec![
            ("deepStandbyEnabled", boolean(true)),
            ("deepStandbyTimeoutMinutes", number(240)),
        ])),
        // the stereo pair: which speaker this station is, its partner's DUID,
        // and its role (a follower only plays music, the leader listens to requests)
        ("stereoPair", object(vec![
            ("channel", one_of(&["left", "right"])),
            ("partnerDeviceId", Schema::new(Node::String)),
            ("role", one_of(&["follower", "leader"])),
        ])
        .optional()),
        ("tvBeta", boolean(false)),
    ]);

    let model = object(vec![
        ("crc", Schema::new(Node::Number)),
        ("fallbackUrls", Schema::new(Node::Array(Box::new(Schema::new(Node::String))))),
        ("format", one_of(&["zip"])),
        ("type", Schema::new(Node::String).optional()),
        ("url", Schema::new(Node::String)),
        ("word", Schema::new(Node::String).optional()),
    ]);
    let rate_limiter_group = object(vec![
        ("countLimit", Schema::new(Node::Number)),
        ("events", Schema::new(Node::Array(Box::new(Schema::new(Node::String))))),
        ("intervalMs", Schema::new(Node::Number)),
    ]);
    let spotter_logging = || object(vec![
        ("spotterLoggingRareEventPercent", number(100)),
        ("spotterLoggingRareEventTailMillis", number(5000)),
        ("spotterLoggingVeryRareEventTailMillis", number(5000)),
    ]);

    let system_config = object(vec![
        ("adbd", object(vec![("network", boolean(true)), ("remoteActivation", boolean(true))])),
        ("addAllEmbeddedEndpoints", boolean(false)),
        ("analyticsEnvironment", object(vec![
            ("quasmodromGroup", string("production")),
            ("quasmodromSubgroup", string("production")),
            ("testBuckets", string(
                "1556982,0,4;1561379,0,36;1547992,0,87;721150,0,79;1525886,0,41;1217762,0,78;945524,0,81;\
                 950035,0,41;1283447,0,48;1423171,0,62;956122,0,95;1549517,0,96;1550823,0,49;1288685,0,77;\
                 1560108,0,48;1287409,0,67;1479393,0,60;1559533,0,28",
            )),
            ("testids", string(
                "1058670_1058695_1058696_1058739_1058743_1058746_1074585_1098487_1118599_1155918_1294733_\
                 1367806_1395542_1420044_1431608_1432181_1450785_1454721_1457247_1457514_1458555_1460807_\
                 1479393_1511604_1525886_1559533_721150",
            )),
        ])),
        ("audioclient", object(vec![("gogol", object(vec![("keepAlive", boolean(true))]))])),
        ("audioInput", object(vec![(
            "modelStorage",
            object(vec![(
                "models",
                Schema::new(Node::Record(Box::new(Schema::new(Node::Record(Box::new(model)))))).default(json!({})),
            )]),
        )])),
        ("audioPlayerCapability", object(vec![
            ("enableMusicSets", boolean(true)),
            ("enableSmartCrossfade", boolean(true)),
        ])),
        ("bioCapability", object(vec![
            ("engineConfig", object(vec![(
                "contextScoring",
                object(vec![
                    ("enabled", boolean(true)),
                    ("guestThreshold", number(0.67)),
                    ("lambda", number(0.3)),
                    ("maxTimeDiffSeconds", number(1140)),
                    ("minEmbeddingCosine", number(0.7)),
                    ("useExpFormula", boolean(true)),
                ]),
            )])),
            ("useOnDeviceClassification", boolean(true)),
        ])),
        ("calld", object(vec![
            ("audioProcessingConfig", object(vec![(
                "preAmplifier",
                object(vec![("enabled", boolean(true)), ("fixedGainFactor", number(72))]),
            )])),
            ("autoGainControl", boolean(true)),
            ("txAgcDigitalCompressionGain", number(72)),
        ])),
        ("comYandexCapabilities", object(vec![
            ("appLaunchCapabilityEnabled", boolean(true)),
            ("detailsCapability", object(vec![("openPurchaseProcessDirectiveEnabled", boolean(true))])),
            ("detailsCapabilityEnabled", boolean(true)),
            ("serialNavigatorCapability", object(vec![
                ("openPurchaseDirectiveEnabled", boolean(true)),
                ("showEpisodeDirectiveEnabled", boolean(true)),
            ])),
            ("serialNavigatorCapabilityEnabled", boolean(true)),
        ])),
        ("comYandexTvHome", object(vec![
            ("detailsProtoHttpEnabled", boolean(true)),
            ("serialNavigatorScreen", object(vec![
                ("enumerationEnabled", boolean(true)),
                ("newSerialNavigatorEnabledV3", boolean(true)),
                ("voiceControlsEnabled", boolean(true)),
            ])),
        ])),
        ("comYandexTvServices", object(vec![("notifications", object(vec![("whitelist", strings(&[]))]))])),
        ("deviceControlPanel", object(vec![("checkAliceProSubscription", boolean(true))])),
        ("experiments", strings(&[])),
        ("fluentBit", object(vec![("samplingRatio", number(0.5))])),
        ("forceWifiReconfigure", boolean(false)),
        ("hybridRequestFactory", object(vec![
            ("hybrid", object(vec![(
                "scenarios",
                object(vec![(
                    "fastCommandScenario",
                    object(vec![(
                        "supportedFrames",
                        strings(&[
                            "personal_assistant.scenarios.bluetooth_off",
                            "personal_assistant.scenarios.bluetooth_on",
                            "personal_assistant.scenarios.player.continue",
                            "personal_assistant.scenarios.player.next_track",
                            "personal_assistant.scenarios.player.pause",
                            "personal_assistant.scenarios.player.previous_track",
                            "personal_assistant.scenarios.sound.louder",
                            "personal_assistant.scenarios.sound.quiter",
                        ]),
                    )]),
                )]),
            )])),
            ("hybridTextInputs", boolean(true)),
            ("useProtobufAnalyticsInfo", boolean(true)),
        ])),
        ("iot", object(vec![
            ("finishSystemDiscoveryAccumulateTimeout", number(120)),
            ("maxFinishSystemDiscoveryAccumulateTimeout", number(180)),
            ("providers", Schema::new(Node::Record(Box::new(Schema::new(Node::Boolean))))
                .default(json!({ "Matter": true, "Zenoh": true }))),
            ("syncApi", boolean(true)),
            ("syncEndpoints", boolean(true)),
            ("syncEndpointsRemoveEnable", boolean(true)),
            ("systemDiscovery", boolean(true)),
        ])),
        ("maind", object(vec![(
            "selfDestroyer",
            object(vec![(
                "additionalMemoryLimitsKb",
                object(vec![("intonationInterruptionModel", number(3400))]),
            )]),
        )])),
        ("matter", object(vec![
            ("enableOnOtaSystemDiscovery", boolean(true)),
            ("enableSystemDiscovery", boolean(true)),
        ])),
        ("mediad", object(vec![("hlsFragmentDistance", number(6)), ("hlsRandomizePlaylists", boolean(true))])),
        ("onlineSpotterEnabled", boolean(true)),
        ("phoneCalls", object(vec![
            ("blockUpdatesOnCall", boolean(true)),
            ("enabled", boolean(true)),
            ("webrtc", object(vec![("enabled", boolean(false))])),
        ])),
        ("qrPaymentConfig", object(vec![
            ("cardDetails", object(vec![
                ("playAfterLogin", boolean(true)),
                ("playAfterPurchase", boolean(true)),
                ("pollingApiEnabled", boolean(false)),
                ("showQrAfterLogin", boolean(true)),
            ])),
            ("checkActiveUserIsOwnerEnabled", boolean(true)),
            ("qrPaymentEnabled", boolean(true)),
            ("redesignEnabledV3", boolean(true)),
        ])),
        ("sendVinsRequestProto", boolean(true)),
        ("shouldCheckWifi", boolean(false)),
        ("smartVolume", object(vec![("enabled", boolean(true))])),
        ("teleme3d", object(vec![(
            "appmetrica",
            object(vec![("plusEvents", strings(&["directiveHandled", "directiveStarted", "directiveCompleted"]))]),
        )])),
        ("telemetry", object(vec![(
            "rateLimiter",
            object(vec![("groups", Schema::new(Node::Array(Box::new(rate_limiter_group))).default(json!([])))]),
        )])),
        ("unbound", object(vec![
            ("checkHosts", strings(&["quasar.yandex.net", "uniproxy.alice.yandex.net", "scbh.yandex.net"])),
            ("enable", boolean(true)),
            ("forwarders", strings(&["77.88.8.1", "77.88.8.8", "77.88.8.88", "77.88.8.2", "1.1.1.1"])),
            ("serverParams", strings(&["msg-cache-size: 2m", "rrset-cache-size: 2m", "username: \"nobody\""])),
        ])),
        ("useVinsRequestProto", boolean(true)),
        ("voiceActivityDetector", object(vec![("eventsEnabled", boolean(true))])),
        ("voiceDialogSettings", object(vec![
            ("backoffUseNew", boolean(true)),
            ("blockUpdateSettingsWhileActiveRequest", boolean(true)),
            ("commandSpotterSettings", object(vec![("spotterLoggingRareEventPercent", number(100))])),
            ("intonationInterruptionSpotterSettings", spotter_logging()),
            ("intonationSpotterSettings", spotter_logging()),
            ("protoAliceApi", object(vec![
                ("clientEventsWhitelist", strings(&["RequestStat"])),
                ("serverEventsWhitelist", strings(&["RequestStatAck"])),
            ])),
            ("recognizer", object(vec![("packSoundBuffer", boolean(true))])),
            ("spotterLoggingRareEventPercent", number(1)),
        ])),
        ("wifiCapability", object(vec![("enabledV2", boolean(true))])),
        ("wifiSettings", object(vec![(
            "generate204",
            object(vec![
                ("failPeriodMs", number(10_000)),
                ("maxFailAttempts", number(3)),
                ("periodOnLastFailAttemptMs", number(30_000)),
                ("periodSec", number(30)),
                ("timeoutMs", number(8000)),
            ]),
        )])),
        ("zigbee", object(vec![(
            "externalTemperatureMeasurement",
            object(vec![("enabled", boolean(true)), ("reportingIntervalSec", number(60))]),
        )])),
    ]);

    object(vec![
        ("accountConfig", account_config),
        ("aliceProSubscription", alice_pro_subscription),
        ("deviceConfig", device_config),
        ("systemConfig", system_config),
    ])
}
