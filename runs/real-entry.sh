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
set +e
# CHECKD_OPTIONS: further options, for runs/otel.
# shellcheck disable=SC2086
timeout -s TERM "$seconds" /checkd run --start-at "$start_at" \
    --first-checks /plan/firsts ${CHECKD_OPTIONS:-} /case/naemon.cfg > /out/stdout.txt 2>&1
set -e

[ -f "$work/naemon.log" ] && cp "$work/naemon.log" /out/naemon.log
[ -f /case/state/trace ] && cp /case/state/trace /out/trace
cp /case/state/start /out/start
