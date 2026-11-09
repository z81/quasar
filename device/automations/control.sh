#!/bin/sh
# Local controls; these commands do not contact the management server.
D=/data/quasar/alice-automations
LOCK=/tmp/alice-automations.lock
running() {
    [ -r "$LOCK/pid" ] || return 1
    p=$(cat "$LOCK/pid")
    case "$p" in ''|*[!0-9]*) return 1;; esac
    kill -0 "$p" 2>/dev/null && tr '\000' ' ' < "/proc/$p/cmdline" | grep -F "$D/alarmd.sh daemon" >/dev/null
}
case "$1" in
    start)
        running && exit 0
        [ -r "$D/alarms.conf" ] && [ -r "$D/runtime.conf" ] || exit 2
        mkdir -p "$D/commands"
        # stdin/stdout/stderr are detached from ADB and the boot caller.
        if command -v nohup >/dev/null 2>&1; then
            nohup sh "$D/alarmd.sh" daemon </dev/null >>"$D/runtime.log" 2>&1 &
        else
            sh "$D/alarmd.sh" daemon </dev/null >>"$D/runtime.log" 2>&1 &
        fi
        ;;
    stop|snooze|shutdown)
        running || exit 0
        mkdir -p "$D/commands"
        printf '%s\n' "$1" > "$D/commands/.$$.new"
        mv "$D/commands/.$$.new" "$D/commands/$$"
        ;;
    status)
        if running; then echo running=1; else echo running=0; fi
        [ -r "$D/active" ] && { printf 'active='; cat "$D/active"; }
        [ -r "$D/snooze" ] && { printf 'snoozed='; cat "$D/snooze"; }
        [ -r "$D/last_error" ] && { printf 'error='; head -c 500 "$D/last_error"; echo; }
        ;;
    *) echo 'Usage: control.sh start|stop|snooze|shutdown|status' >&2; exit 2;;
esac
