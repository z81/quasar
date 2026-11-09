use super::{ops, run, PANEL};
use serde_json::{json, Value};

pub(super) async fn start(
    target: &ops::Target,
    command: &str,
    backup: bool,
) -> Result<Value, String> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let directory = format!("{PANEL}/jobs/{id}");
    run(target, &format!("umask 077; mkdir -p {directory}")).await?;
    let archive = format!("/data/quasar/quasar-panel-backups/packages-{id}.tar.gz");
    let backup_script = if backup {
        format!(
            r#"
mkdir -p /data/quasar/quasar-panel-backups
need=$(du -sk /data/yam {PANEL}/services 2>/dev/null | awk '{{sum += $1}} END {{print sum+4096}}')
free=$(df -k /data | tail -n 1 | awk '{{print $4}}')
case "$free:$need" in *[!0-9:]*|'') echo 'Не удалось проверить свободное место'; exit 1;; esac
[ "$free" -gt "$need" ] || {{ echo 'Недостаточно места для резервной копии'; exit 1; }}
set --
[ ! -e /data/yam ] || set -- "$@" yam
[ ! -e {PANEL}/services ] || set -- "$@" quasar/quasar-panel/packages/services
[ ! -f /data/quasar/rootkeeper.sh ] || set -- "$@" quasar/rootkeeper.sh
if [ "$#" -gt 0 ]; then tar -czf {archive}.partial -C /data "$@" && mv {archive}.partial {archive} || exit 1; fi
printf 'Резервная копия: %s\n' {archive}
"#
        )
    } else {
        String::new()
    };
    let script = format!(
        r#"#!/bin/sh
# quasar-package-job-v1
umask 077
lock=/tmp/quasar-yam-operation.lock
test "$(cat "$lock/job" 2>/dev/null)" = {id} || exit 75
echo $$ > "$lock/pid"
finish() {{ rc=$?; printf '%s\n' "$rc" > {directory}/exit; rm -rf {PANEL}/staging "$lock"; exit "$rc"; }}
trap finish EXIT
trap 'exit 143' TERM INT HUP
set -e
{backup_script}
{command}
"#
    );
    ops::push(target, &format!("{directory}/run.sh"), script.as_bytes()).await?;
    run(
        target,
        &format!(
            r#"lock=/tmp/quasar-yam-operation.lock
if [ -d "$lock" ]; then
 p=$(cat "$lock/pid" 2>/dev/null)
 case "$p" in ''|*[!0-9]*) echo 'Операция YAM ещё запускается'; exit 1;; esac
 if kill -0 "$p" 2>/dev/null; then echo 'Операция YAM ещё выполняется'; exit 1; fi
 rm -rf "$lock"
fi
mkdir "$lock" || exit 1
printf '%s\n' {id} > "$lock/job"
printf '0\n' > "$lock/pid"
nohup /bin/sh {directory}/run.sh > {directory}/output 2>&1 < /dev/null &
worker=$!
if [ -d "$lock" ]; then printf '%s\n' "$worker" > "$lock/pid" 2>/dev/null || true; fi
true
"#
        ),
    )
    .await?;
    Ok(json!({"job":id,"backup":backup.then_some(archive)}))
}

pub(super) async fn read(target: &ops::Target, id: &str) -> Result<Value, String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Некорректный номер операции".into());
    }
    let directory = format!("{PANEL}/jobs/{id}");
    let output = run(target, &format!("test -f {directory}/run.sh && grep -qx '# quasar-package-job-v1' {directory}/run.sh || exit 1\nif [ -f {directory}/exit ]; then printf 'exit:'; cat {directory}/exit; else printf 'running\\n'; fi\ntail -c 24000 {directory}/output 2>/dev/null || true")).await?;
    let (first, text) = output.split_once('\n').unwrap_or((&output, ""));
    let code = first
        .strip_prefix("exit:")
        .and_then(|value| value.parse::<i32>().ok());
    Ok(json!({"job":id,"running":code.is_none(),"exit_code":code,"text":text}))
}
