//! Owner-controlled YAM package operations. The station runs only validated
//! package names and panel-generated jobs; HTTP parameters never become shell.
mod catalog;
mod jobs;
mod services;

use crate::{api::ApiState, auth::AuthManager, device_access::operations as ops};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

const ROOT: &str = "/data/yam";
const PANEL: &str = "/data/quasar/quasar-panel/packages";
const RUN: &str = "/data/yam/bin/yam-run";
const REGISTRY: &str = "https://repo.yse.wtf/v2";
const REGISTRY_KEY: &str = "ed25519:5Rz8F47uftWVU5kLgWYhprkEZ4rpP2kecUge/a7KQas=";

#[derive(Clone)]
struct Context {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
}

pub fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/devices/{duid}/packages", get(read).post(change))
        .with_state(Context { api, auth })
}

#[derive(Default, Deserialize)]
struct Read {
    host: Option<String>,
    port: Option<u16>,
    op: Option<String>,
    job: Option<String>,
}

#[derive(Deserialize)]
struct Change {
    target: Option<ops::Target>,
    action: String,
    package: Option<String>,
    service: Option<String>,
    #[serde(default)]
    yes: bool,
}

#[derive(Clone, Default, Serialize)]
struct Package {
    name: String,
    version: String,
    target: String,
}

#[derive(Default, Serialize)]
struct Status {
    installed: bool,
    owned: bool,
    platform: Option<String>,
    arch: String,
    packages: Vec<Package>,
    services: Vec<services::Service>,
    services_available: bool,
    catalog: Vec<Value>,
    registry: bool,
    message: Option<String>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value != "."
        && value != ".."
}

fn package_name(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2 && parts.iter().all(|part| identifier(part))
}

async fn run(target: &ops::Target, script: &str) -> Result<String, String> {
    let (code, output) = ops::execute(target, script).await?;
    let output = String::from_utf8_lossy(&output).into_owned();
    if code != 0 {
        return Err(format!(
            "Команда завершилась с кодом {code}: {}",
            output
                .chars()
                .rev()
                .take(3000)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        ));
    }
    Ok(output)
}

async fn status(target: &ops::Target, duid: &str, include_catalog: bool) -> Result<Status, String> {
    let script = format!(
        r#"printf 'arch\t'; uname -m
printf 'identity\t'; {{ getprop ro.product.model 2>/dev/null; getprop ro.product.device 2>/dev/null; cat /proc/device-tree/compatible 2>/dev/null; }} | tr '\000\n' '  '; printf '\n'
if [ -x {RUN} ]; then
 printf 'installed\t1\n'
 sed -n "s/^export YAM_PLATFORM='\([^']*\)'$/platform\t\1/p" {RUN}
 if [ -f {ROOT}/db/state.sqlite3 ]; then
  {RUN} list 2>/dev/null | while IFS= read -r row; do printf 'package\t%s\n' "$row"; done
  {RUN} registry list 2>/dev/null | while IFS= read -r row; do printf 'registry\t%s\n' "$row"; done
 fi
fi
if [ "$(cat {ROOT}/.quasar-panel-owner 2>/dev/null)" = {owner} ]; then printf 'owned\t1\n'; fi
true"#,
        owner = ops::quote(&format!("quasar-packages-v1:{duid}"))
    );
    let text = run(target, &script).await?;
    let mut status = Status::default();
    let mut identity = String::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            ["arch", value] => status.arch = value.trim().into(),
            ["identity", value] => identity = value.to_lowercase(),
            ["installed", "1"] => status.installed = true,
            ["owned", "1"] => status.owned = true,
            ["platform", value] if identifier(value) => status.platform = Some((*value).into()),
            ["package", name, version, target] if package_name(name) => {
                status.packages.push(Package {
                    name: (*name).into(),
                    version: (*version).into(),
                    target: (*target).into(),
                })
            }
            ["registry", "official", url] if *url == REGISTRY => status.registry = true,
            _ => {}
        }
    }
    if status.platform.is_none() {
        status.platform = detect_platform(&identity);
    }
    if status.installed {
        match services::list(target).await {
            Ok(values) => {
                status.services = values;
                status.services_available = true;
            }
            Err(error) => status.message = Some(error),
        }
    }
    if include_catalog {
        match catalog::packages(status.platform.as_deref()).await {
            Ok(values) => status.catalog = values,
            Err(error) => status.message = Some(error),
        }
    }
    Ok(status)
}

fn detect_platform(identity: &str) -> Option<String> {
    [
        ("cucumber", "cucumber"),
        ("station-midi", "yandexmidi"),
        ("yandexmidi", "yandexmidi"),
        ("station-mini2", "yandexmini_2"),
        ("station-mini3", "bergamot"),
        ("bergamot", "bergamot"),
        ("yandexstation_2", "yandexstation_2"),
        ("station-max", "yandexstation_2"),
        ("prune", "plum"),
        ("yandexmicro", "yandexmicro"),
        ("mt8167", "jbl_link_music"),
        ("amlogic,axg", "yandexmini"),
        ("mango", "mango"),
        ("orion", "orion"),
        ("pickle", "pickle"),
        ("saturn", "saturn"),
        ("tulip", "yandexstation"),
        ("plum", "plum"),
    ]
    .into_iter()
    .find_map(|(needle, platform)| identity.contains(needle).then(|| platform.into()))
}

async fn read(
    State(context): State<Context>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Query(query): Query<Read>,
) -> Response {
    if let Err(response) = ops::authorize(&context.api, &context.auth, &duid, &headers, false).await
    {
        return response;
    }
    let target = query.host.map(|host| ops::Target {
        host,
        port: query.port.unwrap_or(5555),
    });
    let target = match ops::resolve(&context.api, &duid, target).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let result = if query.op.as_deref() == Some("job") {
        jobs::read(&target, query.job.as_deref().unwrap_or_default()).await
    } else {
        status(&target, &duid, true)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
    };
    reply(result)
}

async fn change(
    State(context): State<Context>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Change>,
) -> Response {
    if let Err(response) = ops::authorize(&context.api, &context.auth, &duid, &headers, true).await
    {
        return response;
    }
    let target = match ops::resolve(&context.api, &duid, body.target.clone()).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _guard = match ops::exclusive(&target).await {
        Ok(guard) => guard,
        Err(error) => return ops::error(StatusCode::CONFLICT, error),
    };
    if let Err(error) = ensure_idle(&target).await {
        return ops::error(StatusCode::CONFLICT, error);
    }
    if let Err(error) = run(&target, &format!("test ! -L /data/quasar && test ! -L /data/quasar/quasar-panel && test ! -L {PANEL} && test ! -L {ROOT} || {{ echo 'Управляемый каталог не должен быть ссылкой'; exit 1; }}")).await { return ops::error(StatusCode::CONFLICT, error); }
    reply(change_inner(&target, &duid, body).await)
}

async fn change_inner(target: &ops::Target, duid: &str, body: Change) -> Result<Value, String> {
    let current = status(target, duid, false).await?;
    if body.action == "bootstrap" {
        if current.installed {
            return Err("YAM уже установлен; используйте обновление".into());
        }
        let platform = current
            .platform
            .ok_or("Платформа колонки не распознана; автоматическая установка недоступна")?;
        let prepared = catalog::bootstrap(target, duid, &platform, &current.arch).await?;
        return jobs::start(target, &prepared, true).await;
    }
    if !current.installed {
        return Err("Сначала установите YAM".into());
    }
    if matches!(
        body.action.as_str(),
        "install"
            | "remove"
            | "uninstall"
            | "service_start"
            | "service_stop"
            | "service_enable"
            | "service_disable"
    ) && !current.services_available
    {
        return Err("Список служб недоступен; изменение пакетов остановлено. Сначала проверьте или восстановите базу YAM.".into());
    }
    let script = match body.action.as_str() {
        "registry" => format!(
            "{RUN} registry add official {} --key {}",
            ops::quote(REGISTRY),
            ops::quote(REGISTRY_KEY)
        ),
        "doctor" | "repair" | "update" => format!("{RUN} {}", body.action),
        "plan" | "install" | "remove" => {
            let name = body
                .package
                .as_deref()
                .filter(|value| package_name(value))
                .ok_or("Некорректное имя пакета")?;
            if body.action == "install" {
                let entries = catalog::packages(current.platform.as_deref()).await?;
                if !entries
                    .iter()
                    .any(|entry| entry["name"] == name && entry["fits"] == true)
                {
                    return Err(
                        "Пакет отсутствует в официальном каталоге или не поддерживает платформу"
                            .into(),
                    );
                }
                if !current.registry {
                    return Err("Сначала подключите официальный реестр".into());
                }
            }
            let stop = if body.action != "plan" {
                services::stop_package(&current.services, name)?
            } else {
                String::new()
            };
            format!(
                "{stop}\n{RUN} {} {} {}",
                body.action,
                if body.action == "install" && body.yes {
                    "--yes"
                } else {
                    ""
                },
                ops::quote(name)
            )
        }
        "uninstall" => {
            if !current.owned {
                return Err("Панель удаляет только YAM, установленный ею для этой колонки".into());
            }
            let mut script = String::new();
            for service in &current.services {
                script.push_str(&services::stop_script(service)?);
            }
            script.push_str(&format!("\ntest \"$(cat {ROOT}/.quasar-panel-owner)\" = {} || exit 1\nrm -rf {PANEL}/services {ROOT}\n", ops::quote(&format!("quasar-packages-v1:{duid}"))));
            script.push_str(&services::remove_hook());
            script
        }
        "service_start" | "service_stop" | "service_enable" | "service_disable" => {
            let id = body.service.as_deref().ok_or("Не выбрана служба")?;
            let service = current
                .services
                .iter()
                .find(|service| service.id == id)
                .ok_or("Служба не найдена")?;
            services::operation(target, &current, service, &body.action).await?
        }
        _ => return Err("Неизвестное действие пакетов".into()),
    };
    jobs::start(
        target,
        &script,
        !matches!(body.action.as_str(), "doctor" | "plan"),
    )
    .await
}

fn reply(result: Result<Value, String>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => ops::error(StatusCode::CONFLICT, error),
    }
}

/// Shared maintenance may include these paths in an owner-only backup. The
/// cleanup script must itself run only after that backup has succeeded.
pub(crate) async fn cleanup_script(target: &ops::Target, duid: &str) -> Result<String, String> {
    let current = status(target, duid, false).await?;
    if !current.owned {
        return Ok("true\n".into());
    }
    if !current.services_available {
        return Err("Не удалось проверить службы YAM; удаление остановлено".into());
    }
    let mut script = String::new();
    for service in &current.services {
        script.push_str(&services::stop_script(service)?);
    }
    script.push_str(&format!("\ntest \"$(cat {ROOT}/.quasar-panel-owner)\" = {} || exit 1\nrm -rf {PANEL}/services {ROOT}\n", ops::quote(&format!("quasar-packages-v1:{duid}"))));
    script.push_str(&services::remove_hook());
    Ok(script)
}

pub(crate) async fn ensure_idle(target: &ops::Target) -> Result<(), String> {
    run(
        target,
        r#"lock=/tmp/quasar-yam-operation.lock
if [ -d "$lock" ]; then
 p=$(cat "$lock/pid" 2>/dev/null)
 case "$p" in ''|0|*[!0-9]*) echo 'Операция YAM ещё запускается'; exit 1;; esac
 if kill -0 "$p" 2>/dev/null; then echo 'Операция YAM ещё выполняется'; exit 1; fi
 rm -rf "$lock"
fi
true"#,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn stop_services_script(target: &ops::Target) -> Result<String, String> {
    ensure_idle(target).await?;
    let exists = run(
        target,
        &format!("if [ -f {ROOT}/db/state.sqlite3 ]; then echo yes; fi\ntrue"),
    )
    .await?;
    if exists.trim() != "yes" {
        return Ok("true\n".into());
    }
    let mut script = String::new();
    for service in services::list(target).await? {
        script.push_str(&services::stop_script(&service)?);
    }
    script.push_str(&services::remove_hook());
    Ok(script)
}

pub(crate) async fn ensure_restore_allowed(target: &ops::Target, duid: &str) -> Result<(), String> {
    run(target, &format!("if [ -e {ROOT} ] || [ -L {ROOT} ]; then test ! -L {ROOT} && test \"$(cat {ROOT}/.quasar-panel-owner 2>/dev/null)\" = {} || {{ echo 'Существующий YAM не принадлежит панели; восстановление остановлено'; exit 1; }}; fi", ops::quote(&format!("quasar-packages-v1:{duid}")))).await.map(|_| ())
}
