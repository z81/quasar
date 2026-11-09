use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::device_access::operations::{self, Target};

const DUMP: &str = "dbus-send --system --print-reply --reply-timeout=5000 --dest=org.bluez / org.freedesktop.DBus.ObjectManager.GetManagedObjects";

fn quoted(value: &str) -> Option<String> {
    let start = value.find('"')?;
    let text = &value[start..];
    // D-Bus and JSON use the same quoting for ordinary Bluetooth device names.
    serde_json::from_str(text)
        .ok()
        .or_else(|| text.strip_prefix('"')?.strip_suffix('"').map(str::to_owned))
}

fn objects(text: &str) -> BTreeMap<String, BTreeMap<String, Value>> {
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    let mut output = BTreeMap::<String, BTreeMap<String, Value>>::new();
    let mut path = String::new();
    let mut interface = String::new();
    for (index, line) in lines.iter().enumerate() {
        let next = lines.get(index + 1).copied().unwrap_or_default();
        if line.starts_with("object path ") && next == "array [" {
            path = quoted(line).unwrap_or_default();
            interface.clear();
        } else if line.starts_with("string ") && next == "array [" {
            interface = quoted(line).unwrap_or_default();
        } else if line.starts_with("string ") && next.starts_with("variant") {
            if !matches!(
                interface.as_str(),
                "org.bluez.Device1" | "org.bluez.Adapter1" | "org.bluez.Battery1"
            ) {
                continue;
            }
            let Some(property) = quoted(line) else {
                continue;
            };
            let words = next.split_whitespace().collect::<Vec<_>>();
            let value = match words.get(1).copied() {
                Some("string") => quoted(next).map(Value::String),
                Some("boolean") => words.get(2).map(|value| Value::Bool(*value == "true")),
                Some("int16" | "int32" | "uint16" | "uint32" | "byte") => words
                    .get(2)
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(Value::from),
                Some("array") => Some(Value::Array(
                    lines
                        .iter()
                        .skip(index + 2)
                        .take_while(|line| **line != "]")
                        .filter_map(|line| quoted(line))
                        .map(Value::String)
                        .collect(),
                )),
                _ => None,
            };
            if let Some(value) = value {
                output
                    .entry(path.clone())
                    .or_default()
                    .insert(property, value);
            }
        }
    }
    output
}

pub(super) async fn status(target: &Target) -> Result<Value, String> {
    let command=format!("command -v dbus-send >/dev/null || exit 2\n{DUMP}\nprintf '\\n@@output\\n'\nbluealsactl status 2>/dev/null\nprintf '\\n@@end\\n'\n{}",super::bluetooth_output::probe_script());
    let (code, output) = operations::execute(target, &command).await?;
    if code == 2 {
        return Ok(
            json!({"available":false,"reason":"В прошивке нет управления Bluetooth по D-Bus","devices":[]}),
        );
    }
    let text = String::from_utf8_lossy(&output);
    let parts = super::probe::sections(&text);
    let mut external = super::bluetooth_output::info(
        parts
            .get("output_setup")
            .map(String::as_str)
            .unwrap_or_default(),
    );
    let (dump, profiles) = text.split_once("@@output").unwrap_or((&text, ""));
    let objects = objects(dump);
    let adapter = objects
        .iter()
        .find(|(path, properties)| !path.contains("/dev_") && properties.contains_key("Powered"));
    let Some((path, adapter)) = adapter else {
        return Ok(
            json!({"available":false,"reason":"Bluetooth-адаптер не ответил через BlueZ","devices":[],"output":external}),
        );
    };
    let mut devices=objects.iter().filter(|(path,_)|path.contains("/dev_")).filter_map(|(_,properties)|{
        let mac=properties.get("Address")?.as_str()?;
        if !valid_mac(mac) {return None;}
        Some(json!({"mac":mac,"name":properties.get("Alias").or_else(||properties.get("Name")).and_then(Value::as_str).unwrap_or(mac),
            "paired":properties.get("Paired").and_then(Value::as_bool).unwrap_or(false),
            "connected":properties.get("Connected").and_then(Value::as_bool).unwrap_or(false),
            "trusted":properties.get("Trusted").and_then(Value::as_bool).unwrap_or(false),
            "rssi":properties.get("RSSI"),"battery":properties.get("Percentage"),"icon":properties.get("Icon"),
            "audio_sink":properties.get("UUIDs").and_then(Value::as_array).is_some_and(|values|values.iter().any(|value|value.as_str()==Some("0000110b-0000-1000-8000-00805f9b34fb")))}))
    }).collect::<Vec<_>>();
    devices.sort_by_key(|device| {
        (
            !device["connected"].as_bool().unwrap_or(false),
            !device["paired"].as_bool().unwrap_or(false),
            device["name"].as_str().unwrap_or_default().to_lowercase(),
        )
    });
    devices.truncate(100);
    let source_available = profiles.to_ascii_lowercase().contains("a2dp-source");
    if path != "/org/bluez/hci0" {
        external["supported"] = json!(false);
        external["reason"] = json!("Маршрут звука требует штатный адаптер hci0");
    }
    Ok(
        json!({"available":true,"adapter":path,"powered":adapter.get("Powered"),"pairable":adapter.get("Pairable"),
        "name":adapter.get("Alias").or_else(||adapter.get("Name")),"discovering":adapter.get("Discovering"),
        "devices":devices,"output_supported":external["supported"],"a2dp_source_available":source_available,
        "output_reason":external["reason"],"output":external}),
    )
}

fn valid_mac(value: &str) -> bool {
    value.len() == 17
        && value.bytes().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn valid_path(value: &str) -> bool {
    value.starts_with("/org/bluez/hci")
        && value.len() < 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_".contains(&byte))
}

pub(super) async fn action(
    target: &Target,
    action: &str,
    mac: Option<&str>,
) -> Result<Value, String> {
    if action == "output_stop" {
        super::bluetooth_output::stop(target).await?;
        return status(target).await;
    }
    let before = status(target).await?;
    if before["available"] != true {
        return Err(before["reason"]
            .as_str()
            .unwrap_or("Bluetooth недоступен")
            .into());
    }
    let adapter = before["adapter"]
        .as_str()
        .filter(|path| valid_path(path))
        .ok_or("Неверный адрес Bluetooth-адаптера")?;
    let external = &before["output"];
    if action == "output_start" {
        if external["supported"] != true {
            return Err(external["reason"]
                .as_str()
                .unwrap_or("Bluetooth-выход недоступен")
                .into());
        }
        if external["active"] == true || external["running"] == true {
            return Err("Сначала верните звук во встроенный динамик".into());
        }
        if before["powered"] != true {
            return Err("Сначала включите Bluetooth".into());
        }
        let mac = mac
            .filter(|value| valid_mac(value))
            .ok_or("Неверный Bluetooth-адрес")?
            .to_ascii_uppercase();
        if !before["devices"].as_array().is_some_and(|devices| {
            devices.iter().any(|device| {
                device["mac"] == mac && device["paired"] == true && device["audio_sink"] == true
            })
        }) {
            return Err(
                "Сначала выполните сопряжение с устройством, поддерживающим приём звука A2DP"
                    .into(),
            );
        }
        super::bluetooth_output::start(target, &mac).await?;
        return status(target).await;
    }
    if (external["active"] == true || external["running"] == true)
        && (action == "power_off"
            || (matches!(action, "forget" | "disconnect")
                && mac.is_some_and(|mac| {
                    external["mac"]
                        .as_str()
                        .is_some_and(|active| active.eq_ignore_ascii_case(mac))
                })))
    {
        return Err("Сначала верните звук во встроенный динамик".into());
    }
    let method = |path: &str, call: &str| {
        format!(
            "dbus-send --system --print-reply --reply-timeout=12000 --dest=org.bluez {} {call}",
            operations::quote(path)
        )
    };
    let command=match action {
        "scan"=>{
            if before["powered"]!=true {return Err("Сначала включите Bluetooth".into());}
            // Discovery belongs to a live D-Bus client; dbus-send would release
            // its session immediately. bluetoothctl is used only for this
            // explicit bounded scan, never for repeated background status.
            "command -v bluetoothctl >/dev/null || exit 2\nbluetoothctl --timeout 5 scan on >/dev/null 2>&1".to_owned()
        }
        "power_on"|"power_off"=>method(adapter,&format!("org.freedesktop.DBus.Properties.Set string:org.bluez.Adapter1 string:Powered variant:boolean:{}",action=="power_on")),
        "pair"|"connect"|"disconnect"|"forget"|"trust"|"untrust"=>{
            let mac=mac.filter(|value|valid_mac(value)).ok_or("Неверный Bluetooth-адрес")?.to_ascii_uppercase();
            if !before["devices"].as_array().is_some_and(|devices|devices.iter().any(|device|device["mac"].as_str().is_some_and(|value|value.eq_ignore_ascii_case(&mac)))) {
                return Err("Устройство отсутствует в списке. Выполните поиск ещё раз".into());
            }
            let path=format!("{adapter}/dev_{}",mac.replace(':',"_"));
            match action {
                "forget"=>method(adapter,&format!("org.bluez.Adapter1.RemoveDevice objpath:{path}")),
                "trust"|"untrust"=>method(&path,&format!("org.freedesktop.DBus.Properties.Set string:org.bluez.Device1 string:Trusted variant:boolean:{}",action=="trust")),
                "pair"=>{
                    let set=|enabled|format!("dbus-send --system --print-reply --reply-timeout=3000 --dest=org.bluez {} org.freedesktop.DBus.Properties.Set string:org.bluez.Adapter1 string:Pairable variant:boolean:{enabled}",operations::quote(adapter));
                    let restore=format!("{} >/dev/null 2>&1",set(before["pairable"]==true));
                    format!("{} >/dev/null || exit 2\ntrap {} EXIT HUP INT TERM\ndbus-send --system --print-reply --reply-timeout=10000 --dest=org.bluez {} org.bluez.Device1.Pair",set(true),operations::quote(&restore),operations::quote(&path))
                }
                _=>method(&path,if action=="connect" {"org.bluez.Device1.Connect"}else{"org.bluez.Device1.Disconnect"}),
            }
        }
        _=>return Err("Неизвестное действие Bluetooth".into()),
    };
    let (code, output) = operations::execute(target, &command).await?;
    let text = String::from_utf8_lossy(&output);
    if code != 0 || text.contains("Error org.bluez") || text.contains("Error org.freedesktop") {
        return Err(if text.contains("Authentication") {
            "Не удалось выполнить сопряжение. Переведите устройство в режим подключения и повторите"
        } else if text.contains("NotReady") {
            "Bluetooth-адаптер не готов. Проверьте, что он включён"
        } else {
            "Действие Bluetooth не подтверждено. Обновите список и проверьте устройство"
        }
        .into());
    }
    status(target).await
}
