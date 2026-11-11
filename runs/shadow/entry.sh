#!/bin/sh
# Runs inside the oracle image: the reference with its Livestatus module,
# and checkd in shadow mode beside it.
#   /in      the case, read-only
#   /shared  the scripted plugins, read-only
#   /checkd  the checkd program
#   /out     checkd's log and the reference's
#   $1       how many seconds
#   $2       a file of objects for checkd instead of the case's, or nothing
set -eu
work=/tmp/oracle
mkdir -p /case/plugins /case/state "$work/checkresults"
cp -r /in/. /case/
cp /shared/plugins/* /case/plugins/
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
broker_module=/usr/lib/naemon/naemon-livestatus/livestatus.so $work/live
SETTINGS
config=/case/naemon.cfg
if [ -n "${2:-}" ]; then
    mkdir -p /tmp/checkd-case
    sed "s|^cfg_file=objects.cfg|cfg_file=/tmp/checkd-case/objects.cfg|" /case/naemon.cfg > /tmp/checkd-case/naemon.cfg
    cp "/in/$2" /tmp/checkd-case/objects.cfg
    config=/tmp/checkd-case/naemon.cfg
fi
date +%s%N > /case/state/start
cd /
timeout -s TERM "$1" naemon /case/naemon.cfg > /out/naemon.txt 2>&1 &
while [ ! -S "$work/live" ]; do sleep 0.1; done
timeout -s TERM "$(( $1 - 1 ))" /checkd run --shadow "$work/live" --log-file /out/checkd.log "$config" \
    > /out/checkd.txt 2>&1 || true
wait || true
cp "$work/naemon.log" /out/naemon.log
