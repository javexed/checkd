#!/bin/sh
# Runs inside the oracle image: checkd on runs/store/case, killed with
# SIGKILL partway, then started again on the same store.
#   /in       the case, read-only
#   /shared   the scripted plugins, read-only
#   /checkd   the checkd program
#   /out      where the logs, the trace and the store's contents go
#   $1        the store: sqlite:<file> or postgres:<connection>
set -eu

store=$1
work=/tmp/oracle
cp -r /in/. /case/
mkdir -p /case/plugins /case/state "$work"
cp /shared/plugins/* /case/plugins/
cat >> /case/naemon.cfg <<SETTINGS

log_file=$work/checkd.log
state_retention_file=$work/retention.dat
command_file=$work/checkd.cmd
SETTINGS

date +%s%N > /case/state/start
start=$(cat /case/state/start)
first=$((start / 1000000000))

# The commands, at their seconds after the first start.
feed() {
    grep -v '^#' /case/commands | while read -r at text; do
        text=$(printf '%s' "$text" | sed "s/{+\([0-9]*\)}/\$(($first+\1))/g")
        text=$(eval "printf '%s' \"$text\"")
        now=$(date +%s%N)
        wait=$(( at * 1000 - (now - start) / 1000000 ))
        [ "$wait" -gt 0 ] && sleep "$(printf '%d.%03d' $((wait / 1000)) $((wait % 1000)))"
        while [ ! -p "$work/checkd.cmd" ]; do sleep 0.1; done
        printf '[%d] %s\n' "$(date +%s)" "$text" > "$work/checkd.cmd"
    done
}

cd /
/checkd run --store "$store" /case/naemon.cfg > /out/stdout-1.txt 2>&1 &
pid=$!
feed &
sleep 15
kill -KILL "$pid"
wait "$pid" 2>/dev/null || true
cp "$work/checkd.log" /out/log-1
/checkd state "$store" > /out/state-1 2>&1 || true

: > "$work/checkd.log"
timeout -s TERM 35 /checkd run --store "$store" /case/naemon.cfg > /out/stdout-2.txt 2>&1 || true
cp "$work/checkd.log" /out/log-2
/checkd state "$store" > /out/state-2 2>&1 || true
cp /case/state/trace /out/trace
cp /case/state/start /out/start
