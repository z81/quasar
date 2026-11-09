#!/bin/sh
# Autonomous local WAV alarms. No network, cloud, server, eval, or model-specific mixer mapping.
D=/data/quasar/alice-automations
LOCK=/tmp/alice-automations.lock
umask 077
log() {
    printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >> "$D/events.log"
    [ "$(wc -c < "$D/events.log")" -le 32768 ] || { tail -n 100 "$D/events.log" > "$D/events.log.new"; mv "$D/events.log.new" "$D/events.log"; }
}
fail() { printf '%s\n' "$*" > "$D/last_error"; log "$*"; }
uptime_seconds() { cut -d. -f1 /proc/uptime; }
read_runtime() { IFS='|' read -r PCM MIXER_DEVICE MIXER_CONTROL REVISION < "$D/runtime.conf"; }

if [ "$1" = ring ]; then
    ID=$2 SOUND=$3 VOLUME=$4 RAMP=$5 MINUTES=$6 OWNER=$7
    read_runtime
    original=$(amixer -D "$MIXER_DEVICE" sget "$MIXER_CONTROL" 2>/dev/null | sed -n 's/.*\[\([0-9][0-9]*\)%\].*/\1/p' | head -n 1)
    case "$original" in ''|*[!0-9]*) fail "Не удалось прочитать громкость ALSA: $MIXER_CONTROL"; exit 3;; esac
    child=
    cleanup() {
        [ -z "$child" ] || { kill "$child" 2>/dev/null; wait "$child" 2>/dev/null; }
        amixer -q -D "$MIXER_DEVICE" sset "$MIXER_CONTROL" "$original%" 2>/dev/null
    }
    trap 'exit 0' TERM INT HUP
    trap cleanup EXIT
    start=$(uptime_seconds)
    previous=-1
    while :; do
        kill -0 "$OWNER" 2>/dev/null || exit 0
        elapsed=$(( $(uptime_seconds) - start ))
        [ "$elapsed" -lt "$((MINUTES * 60))" ] || break
        level=$VOLUME
        if [ "$RAMP" -gt 0 ] && [ "$elapsed" -lt "$RAMP" ]; then level=$((VOLUME * (elapsed + 1) / RAMP)); fi
        [ "$level" -le "$VOLUME" ] || level=$VOLUME
        if [ "$level" != "$previous" ]; then
            amixer -q -D "$MIXER_DEVICE" sset "$MIXER_CONTROL" "$level%" 2>/dev/null || { fail "Не удалось изменить громкость ALSA"; exit 4; }
            previous=$level
        fi
        if [ -z "$child" ] || ! kill -0 "$child" 2>/dev/null; then
            if [ -n "$child" ]; then wait "$child" || { fail "aplay не смог воспроизвести локальный WAV: $SOUND"; exit 5; }; fi
            aplay -q -D "$PCM" "$SOUND" >>"$D/audio.log" 2>&1 &
            child=$!
        fi
        sleep 1
    done
    exit 0
fi

[ "$1" = daemon ] || exit 2
if ! mkdir "$LOCK" 2>/dev/null; then
    # Never kill an unknown process or remove a lock of a live daemon.
    sleep 1
    if sh "$D/control.sh" status | grep -q '^running=1$'; then exit 0; fi
    rm -f "$LOCK/pid"
    rmdir "$LOCK" 2>/dev/null || exit 1
    mkdir "$LOCK" 2>/dev/null || exit 0
fi
mkdir -p "$D/fired" "$D/commands"
rm -f "$D/active" "$D"/commands/*
echo $$ > "$LOCK/pid"
worker= active_id= active_snooze=5
stop_sound() {
    if [ -n "$worker" ]; then kill "$worker" 2>/dev/null; wait "$worker" 2>/dev/null; fi
    worker= active_id=
    rm -f "$D/active"
}
cleanup() { stop_sound; rm -f "$LOCK/pid"; rmdir "$LOCK" 2>/dev/null; }
trap 'exit 0' TERM INT HUP
trap cleanup EXIT
fire() {
    [ -z "$worker" ] || { log "Пропущен $id: уже звонит $active_id"; return; }
    [ -n "$sound" ] || sound="$D/alarm.wav"
    if [ ! -r "$sound" ] || [ "$(head -c 4 "$sound")" != RIFF ]; then fail "Недоступен WAV будильника $id: $sound"; return; fi
    rm -f "$D/last_error"
    active_id=$id active_snooze=$snooze
    printf '%s|%s\n' "$id" "$snooze" > "$D/active"
    sh "$D/alarmd.sh" ring "$id" "$sound" "$volume" "$ramp" "$timeout" $$ &
    worker=$!
    log "Звонит $id"
}
log 'Локальный планировщик запущен'
while :; do
    if [ -n "$worker" ] && ! kill -0 "$worker" 2>/dev/null; then wait "$worker" 2>/dev/null; worker= active_id=; rm -f "$D/active"; fi
    for command_file in "$D"/commands/*; do
        [ -f "$command_file" ] || continue
        command=$(cat "$command_file"); rm -f "$command_file"
        case "$command" in
            stop) stop_sound; rm -f "$D/snooze"; log 'Остановлен пользователем';;
            snooze)
                if [ -n "$active_id" ]; then
                    printf '%s|%s\n' "$active_id" "$(( $(date +%s) + active_snooze * 60 ))" > "$D/snooze.new"
                    mv "$D/snooze.new" "$D/snooze"
                    stop_sound; log 'Будильник отложен'
                fi;;
            shutdown) exit 0;;
        esac
    done
    now=$(date +%s)
    if [ "$now" -lt 1704067200 ]; then fail 'Часы колонки не установлены; расписание ожидает корректного времени'; sleep 30; continue; fi
    snooze_id= snooze_at=0
    [ ! -r "$D/snooze" ] || IFS='|' read -r snooze_id snooze_at < "$D/snooze"
    case "$snooze_at" in ''|*[!0-9]*) rm -f "$D/snooze"; snooze_id=; snooze_at=0;; esac
    snooze_found=0
    if [ -r "$D/alarms.conf" ]; then
        while IFS='|' read -r id enabled hour days one_date zone_file sound volume ramp snooze timeout; do
            [ "$enabled" = 1 ] || continue
            [ "$id" != "$snooze_id" ] || snooze_found=1
            [ -r "$D/tz/$zone_file" ] || { fail "Нет часового пояса для $id"; continue; }
            local_now=$(TZ=":$D/tz/$zone_file" date '+%Y-%m-%d|%H:%M|%u')
            day=${local_now%%|*}; rest=${local_now#*|}; minute=${rest%%|*}; weekday=${rest##*|}
            if [ "$id" = "$snooze_id" ] && [ "$now" -ge "$snooze_at" ]; then
                rm -f "$D/snooze"
                if [ "$((now - snooze_at))" -le 120 ]; then fire; else log "Пропущено отложенное срабатывание $id после остановки runtime"; fi
                continue
            fi
            [ "$minute" = "$hour" ] || continue
            if [ -n "$one_date" ]; then [ "$one_date" = "$day" ] || continue; else case "$days" in *"$weekday"*) ;; *) continue;; esac; fi
            occurrence="$day $hour"
            [ "$(cat "$D/fired/$id" 2>/dev/null)" != "$occurrence" ] || continue
            printf '%s\n' "$occurrence" > "$D/fired/$id.new" && mv "$D/fired/$id.new" "$D/fired/$id" || { fail "Не удалось сохранить срабатывание $id"; continue; }
            fire
        done < "$D/alarms.conf"
    fi
    [ -z "$snooze_id" ] || [ "$snooze_found" = 1 ] || rm -f "$D/snooze"
    sleep 2
done
