#!/bin/sh
# Runs inside the oracle image, for a timed run of the daemon.
#   /in    the case directory, read-only
#   /shared   plugins shared by all cases, read-only
#   /out   where the results are written
#   $1     how many seconds to let Naemon run
#   $2     if given, keep a copy of the status file for every second
# The case is copied to /case, with the plugins at /case/plugins and an
# empty /case/state for them to keep their counts in.
set -eu

seconds=$1
work=/tmp/oracle

cp -r /in/. /case/
mkdir -p /case/plugins /case/state "$work/checkresults"
cp /shared/plugins/* /case/plugins/

# These settings belong to the oracle: they point Naemon at paths that
# exist in the container. Any value a case gives for them is dropped.
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

rm -f /out/naemon.log /out/trace /out/status.dat /out/stdout.txt /out/oracle.version

# A case may give the state Naemon starts from: its retention.dat.
[ -f /in/retention.dat ] && cp /in/retention.dat "$work/retention.dat"

date +%s%N > /case/state/start

# External commands for the run: /in/commands has one to a line,
# "<seconds after the start> <command>", in order of time. "{+N}" in a
# command stands for the second N seconds after the start. Each is
# written to the command file when its time has come, and to
# /out/commands with the millisecond at which it was written.
feed() {
    start=$(cat /case/state/start)
    awk -v start="$start" '
        /^[[:space:]]*(#|$)/ { next }
        {
            at = $1
            sub(/^[^[:space:]]+[[:space:]]+/, "")
            text = $0
            out = ""
            while (match(text, /\{\+[0-9]+\}/)) {
                n = substr(text, RSTART + 2, RLENGTH - 3)
                out = out substr(text, 1, RSTART - 1) sprintf("%d", int(start / 1000000000) + n)
                text = substr(text, RSTART + RLENGTH)
            }
            printf "%d %s\n", at * 1000, out text
        }' /in/commands |
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
        case $text in
            "SPOOL "*)
                # A check result file, dropped where NRDP and the like
                # drop theirs, with its .ok marker.
                name=${text#SPOOL }
                cp "/in/spool/$name" "$work/checkresults/$name"
                : > "$work/checkresults/$name.ok"
                ;;
            *) printf '[%d] %s\n' $((now / 1000000000)) "$text" > "$work/naemon.cmd" ;;
        esac
        printf '%d %s\n' $(( (now - start) / 1000000 )) "$text" >> /out/commands
    done
}

# With a second argument, the status file is copied to
# /out/status/<second of the run>.dat once a second, for looking at how
# the state moved.
watcher=
if [ -n "${2:-}" ]; then
    rm -rf /out/status
    mkdir -p /out/status
    (
        second=0
        while sleep 1; do
            second=$((second + 1))
            [ -f "$work/status.dat" ] && cp "$work/status.dat" "/out/status/$second.dat"
        done
    ) &
    watcher=$!
fi

# Livestatus: with /in/ask, the reference loads its Livestatus module, and
# at each second /in/ask lists every query of /in/queries is put to it.
# In a query, {now} stands for the time it is sent and {now-N} for N
# seconds before. The answers go to /out/answers/<second>/<query>.
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
                second=$(date +%s)
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

rm -f /out/commands
feeder=
if [ -f /in/commands ]; then
    feed &
    feeder=$!
fi
set +e
timeout -s TERM "$seconds" naemon /case/naemon.cfg > /out/stdout.txt 2>&1
[ -n "$feeder" ] && kill "$feeder" 2>/dev/null
[ -n "$watcher" ] && kill "$watcher" 2>/dev/null
[ -n "$asker" ] && kill "$asker" 2>/dev/null
set -e

[ -f "$work/naemon.log" ] && cp "$work/naemon.log" /out/naemon.log
[ -f /case/state/trace ] && cp /case/state/trace /out/trace
cp /case/state/start /out/start
[ -f "$work/status.dat" ] && cp "$work/status.dat" /out/status.dat
[ -f "$work/retention.dat" ] && cp "$work/retention.dat" /out/retention.dat
for f in host-perfdata service-perfdata; do
    if [ -f "$work/$f" ]; then cp "$work/$f" "/out/$f"; fi
done
naemon --version | head -n 1 > /out/oracle.version
