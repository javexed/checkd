#!/bin/sh
# Runs inside the oracle image; see runs/load/run.
#   $1 engine (naemon | checkd), $2 hosts, $3 services per host,
#   $4 check interval in seconds, $5 seconds to run
set -u
engine=$1 hosts=$2 per=$3 interval=$4 seconds=$5
work=/tmp/load
mkdir -p "$work/checkresults"
cd "$work"
# The no-op plugin: one byte to a file per call, so that calls can be counted.
printf '#!/bin/sh\nprintf . >> %s/calls\n' "$work" > tick
chmod +x tick
: > calls
cat > naemon.cfg <<CFG
cfg_file=$work/objects.cfg
interval_length=1
log_file=$work/engine.log
command_file=$work/engine.cmd
check_result_path=$work/checkresults
temp_path=$work
lock_file=$work/engine.pid
query_socket=$work/engine.qh
object_cache_file=$work/objects.cache
precached_object_file=$work/objects.precache
status_file=$work/status.dat
state_retention_file=$work/retention.dat
retain_state_information=0
log_initial_states=0
execute_host_checks=0
# With a cache horizon longer than the interval, every other check is
# passed over (both engines do so); the load test wants every check run.
cached_service_check_horizon=0
cached_host_check_horizon=0
broker_module=/usr/lib/naemon/naemon-livestatus/livestatus.so $work/live
CFG
{
    printf 'define command {\n command_name ok\n command_line /bin/true\n}\n'
    printf 'define command {\n command_name tick\n command_line %s/tick\n}\n' "$work"
    printf 'define timeperiod {\n timeperiod_name always\n alias always\n'
    for d in sunday monday tuesday wednesday thursday friday saturday; do printf ' %s 00:00-24:00\n' "$d"; done
    printf '}\n'
    printf 'define contact {\n contact_name nobody\n host_notification_period always\n service_notification_period always\n host_notification_commands ok\n service_notification_commands ok\n}\n'
    h=0
    while [ "$h" -lt "$hosts" ]; do
        printf 'define host {\n host_name h%d\n address 192.0.2.%d\n check_command ok\n max_check_attempts 1\n check_period always\n contacts nobody\n}\n' "$h" $((h % 250 + 1))
        s=0
        while [ "$s" -lt "$per" ]; do
            printf 'define service {\n host_name h%d\n service_description s%d\n check_command tick\n max_check_attempts 1\n check_interval %d\n retry_interval %d\n check_period always\n contacts nobody\n notifications_enabled 0\n}\n' "$h" "$s" "$interval" "$interval"
            s=$((s + 1))
        done
        h=$((h + 1))
    done
} > objects.cfg

case $engine in
naemon) naemon naemon.cfg > "$work/stdout" 2>&1 & ;;
checkd) /checkd run --store none --http 127.0.0.1:6559 naemon.cfg > "$work/stdout" 2>&1 & ;;
esac
pid=$!
for _ in $(seq 150); do [ -S "$work/live" ] && break; sleep 0.2; done
[ -S "$work/live" ] || { echo "$engine did not start:"; tail -5 "$work/stdout"; exit 1; }
ask() { printf '%b\nOutputFormat: csv\n\n' "$1" | unixcat "$work/live"; }

# A first round of checks, then the measured span. Checks a second: calls
# of the plugin over the span.
sleep $(( interval < 15 ? interval : 15 ))
first=$(ask 'GET services\nStats: sum last_check\nStats: state >= 0' | cut -d';' -f2)
t0=$(date +%s)
c0=$(stat -c %s calls)
# Halfway, how many plugins run at once, and whether the engine's own
# process is busy (its CPU time over a second).
sleep $((seconds / 2))
children=$(grep -l "^PPid:[[:space:]]*$pid\$" /proc/[0-9]*/status 2>/dev/null | wc -l)
[ -n "${LOAD_DEBUG:-}" ] && grep -l "^PPid:[[:space:]]*$pid\$" /proc/[0-9]*/status 2>/dev/null | head -400 | xargs grep -h "^State:" | sort | uniq -c >&2
u0=$(awk '{print $14 + $15}' /proc/"$pid"/stat); sleep 1; u1=$(awk '{print $14 + $15}' /proc/"$pid"/stat)
busy=$((u1 - u0))
rss_half=$(grep -E '^VmRSS' /proc/"$pid"/status | awk '{print $2}')
[ "$engine" = checkd ] && [ -n "${LOAD_DEBUG:-}" ] && curl -s http://127.0.0.1:6559/metrics | grep -E "^checkd_(commands_running|checks_waiting|checks_total)" >&2
sleep $((seconds - seconds / 2 - 1))
t1=$(date +%s)
c1=$(stat -c %s calls)
lat=$(ask 'GET services\nStats: avg latency\nStats: max latency')
stale=$(ask "GET services\nStats: last_check < $((t1 - 2 * interval))")
rss=$(grep -E '^VmHWM' /proc/"$pid"/status | awk '{print $2}')
cpu=$(awk '{print $14 + $15}' /proc/"$pid"/stat)
ticks=$(getconf CLK_TCK)
kill "$pid"
echo "$engine services=$first checks_per_s=$(( (c1 - c0) / (t1 - t0) )) latency_avg_max=$lat behind=$stale rss_mb=$((rss / 1024)) rss_half_mb=$((rss_half / 1024)) rss_end_mb=$(( $(grep -E "^VmRSS" /proc/"$pid"/status | awk "{print \$2}") / 1024 )) cpu_s=$((cpu / ticks)) running=$children busy_pct=$busy span_s=$((t1 - t0))"
