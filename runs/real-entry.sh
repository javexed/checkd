#!/bin/sh
# Runs inside the oracle image, for a timed run of checkd on a scenario.
# It sets the case up the way oracle/play-entry.sh does for the reference,
# so that both see the same paths, the same shell and the same plugins.
#   /in       the scenario directory, read-only
#   /shared   plugins shared by all scenarios, read-only
#   /plan     `seconds`, `start` and `firsts` for this scenario, read-only
#   /checkd   the checkd program
#   /out      where the results are written
set -eu

work=/tmp/oracle

cp -r /in/. /case/
rm -rf /case/expected
mkdir -p /case/plugins /case/state "$work/checkresults"
cp /shared/plugins/* /case/plugins/

owned='log_file|precached_object_file|check_result_path|lock_file|temp_path'
owned="$owned|status_file|state_retention_file|object_cache_file|command_file|query_socket"
grep -v -E "^[[:space:]]*(${owned})[[:space:]]*=" /in/naemon.cfg > /case/naemon.cfg || true
cat >> /case/naemon.cfg <<SETTINGS

log_file=$work/naemon.log
check_result_path=$work/checkresults
lock_file=$work/naemon.pid
temp_path=$work
status_file=$work/status.dat
state_retention_file=$work/retention.dat
object_cache_file=$work/objects.cache
command_file=$work/naemon.cmd
query_socket=$work/naemon.qh
SETTINGS

rm -f /out/naemon.log /out/trace /out/stdout.txt /out/start
read -r seconds < /plan/seconds

# checkd is told to take this moment for the one the reference's run
# began at. The time of day and the day of the week are then the same for
# both, to the millisecond, and so are the times in checkd's log. The
# plugins count from the real moment, as they did for the reference.
read -r start_at < /plan/start

cd /
date +%s%N > /case/state/start

# The external commands the reference was sent, at the same milliseconds
# after the start, with checkd's time in front.
feed() {
    start=$(cat /case/state/start)
    while read -r at text; do
        now=$(date +%s%N)
        wait=$(( at - (now - start) / 1000000 ))
        if [ "$wait" -gt 0 ]; then
            sleep "$(printf '%d.%03d' $((wait / 1000)) $((wait % 1000)))"
        fi
        tries=0
        while [ ! -p "$work/naemon.cmd" ] && [ "$tries" -lt 50 ]; do
            sleep 0.1
            tries=$((tries + 1))
        done
        now=$(date +%s%N)
        stamp=$(( (start_at + (now - start) / 1000000) / 1000 ))
        case $text in
            "SPOOL "*)
                name=${text#SPOOL }
                cp "/in/spool/$name" "$work/checkresults/$name"
                : > "$work/checkresults/$name.ok"
                ;;
            *) printf '[%d] %s\n' "$stamp" "$text" > "$work/naemon.cmd" ;;
        esac
    done < /in/expected/commands
}
# Livestatus, as oracle/play-entry.sh does it for the reference: the same
# broker_module line, and the queries of /in/queries at the seconds of
# /in/ask, with checkd's time for {now}.
asker=
if [ -f /in/ask ] && [ -d /in/queries ]; then
    echo "broker_module=/usr/lib/naemon/naemon-livestatus/livestatus.so $work/live" >> /case/naemon.cfg
    rm -rf /out/answers
    (
        start=$(cat /case/state/start)
        grep -v '^#' /in/ask | while read -r at; do
            now=$(date +%s%N)
            wait=$(( at * 1000 - (now - start) / 1000000 ))
            [ "$wait" -gt 0 ] && sleep "$(printf '%d.%03d' $((wait / 1000)) $((wait % 1000)))"
            mkdir -p "/out/answers/$at"
            for query in /in/queries/*; do
                name=$(basename "$query")
                now=$(date +%s%N)
                second=$(( (start_at + (now - start) / 1000000) / 1000 ))
                awk -v now="$second" '{
                    while (match($0, /\{now(-[0-9]+)?\}/)) {
                        n = substr($0, RSTART + 4, RLENGTH - 5)
                        $0 = substr($0, 1, RSTART - 1) (now + n) substr($0, RSTART + RLENGTH)
                    }
                    print
                }' "$query" > "/tmp/q-$name"
                printf '\n' >> "/tmp/q-$name"
                unixcat "$work/live" < "/tmp/q-$name" > "/out/answers/$at/$name" 2>&1 || true
            done
        done
    ) &
    asker=$!
fi
feeder=
if [ -f /in/expected/commands ]; then
    feed &
    feeder=$!
fi
set +e
# CHECKD_OPTIONS: further options, for runs/otel.
# shellcheck disable=SC2086
timeout -s TERM "$seconds" /checkd run --start-at "$start_at" \
    --first-checks /plan/firsts ${CHECKD_OPTIONS:-} /case/naemon.cfg > /out/stdout.txt 2>&1
[ -n "$feeder" ] && kill "$feeder" 2>/dev/null
[ -n "$asker" ] && kill "$asker" 2>/dev/null
set -e

[ -f "$work/naemon.log" ] && cp "$work/naemon.log" /out/naemon.log
[ -f /case/state/trace ] && cp /case/state/trace /out/trace
cp /case/state/start /out/start
for f in host-perfdata service-perfdata; do
    if [ -f "$work/$f" ]; then cp "$work/$f" "/out/$f"; fi
done
