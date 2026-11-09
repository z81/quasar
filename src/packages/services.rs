use super::{ops, run, Status, PANEL, ROOT};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Specification {
    exec: Vec<String>,
    env: BTreeMap<String, String>,
    restart: String,
    after: Vec<String>,
    oneshot: bool,
}

#[derive(Clone, Serialize)]
pub(super) struct Service {
    pub id: String,
    pub package: String,
    pub name: String,
    pub command: Vec<String>,
    pub running: bool,
    pub enabled: bool,
    pub managed: bool,
    pub supported: bool,
    pub reason: Option<String>,
    #[serde(skip)]
    specification: Specification,
}

pub(super) async fn list(target: &ops::Target) -> Result<Vec<Service>, String> {
    let database = format!("{ROOT}/db/state.sqlite3");
    let before = run(target, &format!("test ! -d /tmp/quasar-yam-operation.lock && test -f {database} && sha256sum {database}")).await?;
    let bytes = ops::pull(target, &database, 16 * 1024 * 1024).await?;
    let after = run(target, &format!("sha256sum {database}")).await?;
    if before.trim() != after.trim() {
        return Err("База пакетов изменилась; обновите список служб".into());
    }
    let directory =
        std::env::temp_dir().join(format!("quasar-yam-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir(&directory)
        .await
        .map_err(|_| "Не удалось подготовить снимок базы YAM")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|_| "Не удалось защитить снимок YAM")?;
    }
    let path = directory.join("state.sqlite3");
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|_| "Не удалось сохранить снимок базы YAM")?;
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<Service>, String> {
        let db =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|_| "Не удалось прочитать базу YAM")?;
        db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
            .map_err(|_| "Неверная база YAM")?;
        let mut statement = db
            .prepare("SELECT package_name,name,spec_yaml FROM services LIMIT 1000")
            .map_err(|_| "Эта версия базы YAM не поддерживает список служб")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| "Не удалось прочитать службы YAM")?;
        let mut services = Vec::new();
        for row in rows {
            let (package, name, yaml) = row.map_err(|_| "Повреждённая запись службы YAM")?;
            if !super::package_name(&package) || !super::identifier(&name) || yaml.len() > 65536 {
                continue;
            }
            let specification: Specification =
                serde_yaml::from_str(&yaml).map_err(|_| "Неверное описание службы YAM")?;
            let reason = validate(&specification).err();
            services.push(Service {
                id: format!("{}~{name}", package.replace('/', "~")),
                package,
                name,
                command: specification.exec.clone(),
                specification,
                running: false,
                enabled: false,
                managed: false,
                supported: reason.is_none(),
                reason,
            });
        }
        Ok(services)
    })
    .await
    .map_err(|_| "Не удалось разобрать базу YAM".to_owned())
    .and_then(|value| value);
    let _ = tokio::fs::remove_dir_all(directory).await;
    let mut services = result?;
    let mut script = String::new();
    for service in &services {
        let id = &service.id;
        script.push_str(&format!("[ ! -f {PANEL}/services/boot/{id} ] || printf 'enabled:{id}\\n'\n[ ! -f {PANEL}/services/{id}.sh ] || {{ grep -qx '# quasar-package-service-v1' {PANEL}/services/{id}.sh && printf 'managed:{id}\\n'; }}\n"));
        script.push_str(&format!(
            "if ( {} ); then printf 'running:{id}\\n'; fi\n",
            identity(service)
        ));
    }
    if !script.is_empty() {
        script.push_str("true\n");
        let flags = run(target, &script).await?;
        for service in &mut services {
            service.running = flags
                .lines()
                .any(|line| line == format!("running:{}", service.id));
            service.enabled = flags
                .lines()
                .any(|line| line == format!("enabled:{}", service.id));
            service.managed = flags
                .lines()
                .any(|line| line == format!("managed:{}", service.id));
        }
    }
    Ok(services)
}

fn validate(specification: &Specification) -> Result<(), String> {
    let Some(executable) = specification.exec.first() else {
        return Err("В описании нет команды запуска".into());
    };
    if executable.starts_with('/')
        || executable
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || executable.contains('$')
    {
        return Err("Исполняемый файл должен находиться внутри пакета".into());
    }
    if specification.exec.len() > 64
        || specification
            .exec
            .iter()
            .any(|arg| arg.len() > 4096 || arg.chars().any(char::is_control))
    {
        return Err("Некорректные аргументы запуска".into());
    }
    if !matches!(
        specification.restart.as_str(),
        "" | "never" | "always" | "on-failure"
    ) {
        return Err("Неизвестная политика перезапуска".into());
    }
    if specification
        .after
        .iter()
        .any(|dependency| dependency != "network")
    {
        return Err("Службе нужен порядок запуска, который панель пока не поддерживает".into());
    }
    for (key, value) in &specification.env {
        if key.is_empty()
            || key.len() > 64
            || !key
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
            || key.as_bytes()[0].is_ascii_digit()
            || key.starts_with("YAM_")
            || value.len() > 4096
            || value.contains('\0')
        {
            return Err("Некорректное окружение службы".into());
        }
    }
    Ok(())
}

fn identity(service: &Service) -> String {
    let id = &service.id;
    format!(
        r#"read p born 2>/dev/null < /tmp/quasar-yam-{id}.pid || exit 1
case "$p:$born" in *[!0-9:]*|'') exit 1;; esac
[ "$p" -gt 1 ] && [ -r /proc/$p/stat ] || exit 1
[ "$(awk '{{print $22}}' /proc/$p/stat)" = "$born" ] || exit 1
[ "$(awk '{{print $5}}' /proc/$p/stat)" = "$p" ] || exit 1
tr '\000' '\n' < /proc/$p/cmdline | grep -Fqx {PANEL}/services/{id}.sh || exit 1
kill -0 "$p" 2>/dev/null"#
    )
}

pub(super) fn stop_script(service: &Service) -> Result<String, String> {
    if !service.managed {
        return Ok(String::new());
    }
    Ok(format!(
        r#"
if ( {identity} ); then
 read p born < /tmp/quasar-yam-{id}.pid
 kill -TERM -- -"$p" 2>/dev/null || true
 for attempt in 1 2 3 4 5; do kill -0 "$p" 2>/dev/null || break; sleep 1; done
 if ( {identity} ); then kill -KILL -- -"$p" 2>/dev/null || true; fi
fi
rm -f /tmp/quasar-yam-{id}.pid /tmp/quasar-yam-{id}.supervisor.pid
rmdir /tmp/quasar-yam-{id}.lock 2>/dev/null || true
"#,
        id = service.id,
        identity = identity(service)
    ))
}

pub(super) fn stop_package(services: &[Service], package: &str) -> Result<String, String> {
    let mut script = String::new();
    for service in services
        .iter()
        .filter(|service| service.package == package && service.managed)
    {
        script.push_str(&stop_script(service)?);
        script.push_str(&format!("rm -f {PANEL}/services/boot/{id} {PANEL}/services/{id}.sh\nprintf 'Служба {id} остановлена; после обновления включите её и автозапуск заново.\\n'\n", id=service.id));
    }
    Ok(script)
}

async fn launcher(
    target: &ops::Target,
    current: &Status,
    service: &Service,
) -> Result<String, String> {
    validate(&service.specification)?;
    let platform = current
        .platform
        .as_deref()
        .ok_or("Не определена платформа YAM")?;
    let profile = run(
        target,
        &format!("cat {ROOT}/platforms/{}.yaml", ops::quote(platform)),
    )
    .await?;
    let profile: serde_json::Value =
        serde_yaml::from_str(&profile).map_err(|_| "Не удалось прочитать профиль YAM")?;
    let mut env = BTreeMap::from([
        ("YAM_ROOT".to_owned(), ROOT.to_owned()),
        ("YAM_PLATFORM".into(), platform.into()),
        (
            "YAM_PKG".into(),
            format!("{ROOT}/pkgs/{}/current", service.package),
        ),
    ]);
    for (key, field) in [("YAM_ETC", "etc"), ("YAM_VAR", "var"), ("YAM_RUN", "run")] {
        let base = profile["paths"][field]
            .as_str()
            .ok_or("В профиле YAM нет путей службы")?;
        if !base.starts_with('/')
            || base.split('/').any(|part| part == "..")
            || base.chars().any(char::is_control)
            || !(base == "/tmp/yam" || base.starts_with("/data/yam/"))
        {
            return Err("Пути профиля YAM вне управляемого каталога".into());
        }
        // Match YAM's own postinstall environment. Package manifests append
        // their namespace where necessary; the profile paths are not rewritten.
        env.insert(key.into(), base.to_owned());
    }
    let expand = |value: &str| -> Result<String, String> {
        let mut value = value.to_owned();
        for (key, replacement) in &env {
            value = value.replace(&format!("${{{key}}}"), replacement);
        }
        if value.contains("${") {
            return Err("В службе есть неизвестная переменная окружения".into());
        }
        Ok(value)
    };
    let arguments = service
        .specification
        .exec
        .iter()
        .skip(1)
        .map(|arg| expand(arg).map(|arg| ops::quote(&arg)))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let mut exports = String::new();
    for (key, value) in &env {
        exports.push_str(&format!("export {key}={}\n", ops::quote(value)));
    }
    for (key, value) in &service.specification.env {
        exports.push_str(&format!("export {key}={}\n", ops::quote(&expand(value)?)));
    }
    let restart = if service.specification.oneshot {
        "never"
    } else {
        service.specification.restart.as_str()
    };
    let condition = match restart {
        "always" => "false",
        "on-failure" => "[ \"$rc\" = 0 ]",
        _ => "true",
    };
    let network = if service
        .specification
        .after
        .iter()
        .any(|value| value == "network")
    {
        "for wait in $(seq 1 60); do awk '$2 == \"00000000\" {found=1} END {exit !found}' /proc/net/route && break; sleep 1; done\n"
    } else {
        ""
    };
    Ok(format!(
        r#"#!/bin/sh
# quasar-package-service-v1
umask 077
mkdir /tmp/quasar-yam-{id}.lock 2>/dev/null || exit 0
trap 'rm -f /tmp/quasar-yam-{id}.pid; rmdir /tmp/quasar-yam-{id}.lock 2>/dev/null' EXIT
{exports}
mkdir -p "$YAM_ETC" "$YAM_VAR" "$YAM_RUN"
pkg=$(readlink -f "$YAM_PKG")
case "$pkg" in {ROOT}/pkgs/{package}/*) ;; *) exit 1;; esac
exe=$(readlink -f "$YAM_PKG"/{executable})
case "$exe" in "$pkg"/*) ;; *) exit 1;; esac
[ -x "$exe" ] || exit 1
cd "$pkg" || exit 1
printf '%s %s\n' "$$" "$(awk '{{print $22}}' /proc/$$/stat)" > /tmp/quasar-yam-{id}.pid
child=
trap 'if [ -n "$child" ]; then kill -TERM "$child" 2>/dev/null; wait "$child"; fi; exit 143' TERM INT HUP
{network}
failures=0
while :; do
 started=$(date +%s)
 "$exe" {arguments} &
 child=$!
 wait "$child"
 rc=$?
 child=
 {condition} && exit "$rc"
 [ $(( $(date +%s) - started )) -lt 60 ] || failures=0
 failures=$((failures+1))
 [ "$failures" -lt 5 ] || exit "$rc"
 sleep 3
done
"#,
        package = service.package,
        executable = ops::quote(&service.specification.exec[0]),
        id = service.id
    ))
}

pub(super) async fn operation(
    target: &ops::Target,
    current: &Status,
    service: &Service,
    action: &str,
) -> Result<String, String> {
    let id = &service.id;
    if action == "service_stop" {
        return stop_script(service);
    }
    if action == "service_disable" {
        return Ok(format!("rm -f {PANEL}/services/boot/{id}"));
    }
    let script = launcher(target, current, service).await?;
    let staging = format!("{PANEL}/staging/service-{}", uuid::Uuid::new_v4().simple());
    run(target, &format!("umask 077; mkdir -p {PANEL}/staging")).await?;
    ops::push(target, &staging, script.as_bytes()).await?;
    let prepare = format!("mkdir -p {PANEL}/services/boot\nif [ -e {PANEL}/services/{id}.sh ]; then grep -qx '# quasar-package-service-v1' {PANEL}/services/{id}.sh || exit 1; fi\ncp {staging} {PANEL}/services/{id}.new\nchmod 700 {PANEL}/services/{id}.new\nmv {PANEL}/services/{id}.new {PANEL}/services/{id}.sh\nrm -f {staging}\n");
    let start = format!("if ! ( {} ); then\nrm -f /tmp/quasar-yam-{id}.supervisor.pid\nrmdir /tmp/quasar-yam-{id}.lock 2>/dev/null || true\nstart-stop-daemon -S -b -m -p /tmp/quasar-yam-{id}.supervisor.pid -x /bin/sh -- {PANEL}/services/{id}.sh > /tmp/quasar-yam-{id}.log 2>&1\nfi\n", identity(service));
    if action == "service_start" {
        return Ok(format!("{prepare}\n{start}"));
    }
    let boot = format!(
        r#"#!/bin/sh
# quasar-package-boot-v1
for marker in {PANEL}/services/boot/*; do
 [ -f "$marker" ] || continue
 id=${{marker##*/}}
 case "$id" in *[!a-z0-9._~-]*) continue;; esac
 script={PANEL}/services/$id.sh
 [ -x "$script" ] && grep -qx '# quasar-package-service-v1' "$script" || continue
 if read p born 2>/dev/null < /tmp/quasar-yam-$id.pid; then
  case "$p:$born" in *[!0-9:]*|'') ;; *)
   if [ -r /proc/$p/stat ] && [ "$(awk '{{print $22}}' /proc/$p/stat)" = "$born" ] && tr '\000' '\n' < /proc/$p/cmdline | grep -Fqx "$script"; then continue; fi;;
  esac
 fi
 rm -f /tmp/quasar-yam-$id.supervisor.pid
 rmdir /tmp/quasar-yam-$id.lock 2>/dev/null || true
 start-stop-daemon -S -b -m -p /tmp/quasar-yam-$id.supervisor.pid -x /bin/sh -- "$script" > /tmp/quasar-yam-$id.log 2>&1
done
"#
    );
    let boot_stage = format!("{PANEL}/staging/boot-{}", uuid::Uuid::new_v4().simple());
    ops::push(target, &boot_stage, boot.as_bytes()).await?;
    Ok(format!("{prepare}\ncp {boot_stage} {PANEL}/boot.sh\nchmod 700 {PANEL}/boot.sh\nrm -f {boot_stage}\n{}\ntouch {PANEL}/services/boot/{id}\n", hook(false)))
}

fn hook(remove: bool) -> String {
    let insert = if remove {
        String::new()
    } else {
        format!(
            "[ ! -x {PANEL}/boot.sh ] || {PANEL}/boot.sh >/dev/null 2>&1 & # quasar-packages-hook"
        )
    };
    format!(
        r#"(
mkdir /tmp/quasar-rootkeeper-edit.lock 2>/dev/null || {{ echo 'Автозапуск сейчас изменяется другой операцией'; exit 1; }}
trap 'rmdir /tmp/quasar-rootkeeper-edit.lock' EXIT
if [ ! -f /data/quasar/rootkeeper.sh ] || [ -L /data/quasar/rootkeeper.sh ]; then echo 'Не найден поддерживаемый rootkeeper'; exit 1; fi
cp -p /data/quasar/rootkeeper.sh /data/quasar/rootkeeper.sh.packages-backup || exit 1
awk -v hook={insert} 'NR==1 {{print; if(length(hook)) print hook; next}} !/#[ ]quasar-packages-hook$/ {{print}}' /data/quasar/rootkeeper.sh > /data/quasar/rootkeeper.sh.packages-new && chmod 755 /data/quasar/rootkeeper.sh.packages-new && mv /data/quasar/rootkeeper.sh.packages-new /data/quasar/rootkeeper.sh
)
"#,
        insert = ops::quote(&insert)
    )
}

pub(super) fn remove_hook() -> String {
    hook(true)
}
