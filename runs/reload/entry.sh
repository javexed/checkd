#!/bin/sh
# Runs inside the oracle image; see runs/reload/run.
#   /checkd  the checkd program
#   /out     where the log goes
#   $1       sqlite or none
set -u
work=/tmp/reload
mkdir -p "$work"
cd "$work"
case $1 in
sqlite) store="sqlite:$work/checkd.sqlite" ;;
*) store=none ;;
esac

cat > naemon.cfg <<CFG
cfg_file=$work/objects.cfg
interval_length=1
log_file=/out/checkd.log
command_file=$work/checkd.cmd
state_retention_file=$work/retention.dat
retain_state_information=1
log_initial_states=1
CFG

base() {
    cat <<'CFG'
define command {
    command_name    ok
    command_line    /bin/true
}
define timeperiod {
    timeperiod_name always
    alias           always
    sunday 00:00-24:00
    monday 00:00-24:00
    tuesday 00:00-24:00
    wednesday 00:00-24:00
    thursday 00:00-24:00
    friday 00:00-24:00
    saturday 00:00-24:00
}
define host {
    host_name       one
    address         192.0.2.70
    check_command   ok
    max_check_attempts 1
    check_period    always
    active_checks_enabled 0
}
CFG
}
service() {
    cat <<'CFG'
define service {
    host_name       one
    service_description probe
    check_command   ok
    max_check_attempts 1
    check_period    always
    active_checks_enabled 0
}
CFG
}
second() {
    cat <<'CFG'
define host {
    host_name       two
    address         192.0.2.71
    check_command   ok
    max_check_attempts 1
    check_period    always
    active_checks_enabled 0
}
CFG
}

failures=0
check() {
    # check <what> <got> <wanted>
    if [ "$2" = "$3" ]; then
        echo "ok    $1"
    else
        echo "FAIL  $1: got '$2', wanted '$3'"
        failures=$((failures + 1))
    fi
}
ask() {
    printf '%b\n\n' "$1" | unixcat "$work/live" 2>/dev/null | tr '\n' ' ' | sed 's/ $//'
}
command() {
    printf '[%d] %s\n' "$(date +%s)" "$1" > "$work/checkd.cmd"
}
logged() {
    grep -c -- "$1" /out/checkd.log 2>/dev/null || true
}
# Waits until the log has a line, at most ten seconds.
wait_log() {
    for _ in $(seq 50); do
        [ "$(logged "$1")" -ge "${2:-1}" ] && return 0
        sleep 0.2
    done
    return 1
}

{ base; service; } > objects.cfg
/checkd run --store "$store" --livestatus "$work/live" naemon.cfg > /out/stdout.txt 2>&1 &
pid=$!
for _ in $(seq 50); do [ -S "$work/live" ] && [ -p "$work/checkd.cmd" ] && break; sleep 0.2; done

# 1. A state, a comment and a downtime.
command "PROCESS_SERVICE_CHECK_RESULT;one;probe;2;broken"
command "ADD_SVC_COMMENT;one;probe;1;ann;kept over a reload"
command "ADD_HOST_COMMENT;one;1;ann;on the host"
command "SCHEDULE_HOST_DOWNTIME;one;$(date +%s);$(( $(date +%s) + 3600 ));1;0;0;ann;planned"
sleep 1.5
check "before: the service is critical" "$(ask 'GET services\nColumns: state plugin_output')" "2;broken"
check "before: one host" "$(ask 'GET hosts\nColumns: name')" "one"

# 2. A host more, and SIGHUP.
{ base; service; second; } > objects.cfg
kill -HUP "$pid"
wait_log "Caught SIGHUP, restarting" || echo "FAIL  no restart after SIGHUP"
for _ in $(seq 50); do [ "$(ask 'GET hosts\nColumns: name')" = "one two" ] && break; sleep 0.2; done
check "reload: both hosts are there" "$(ask 'GET hosts\nColumns: name')" "one two"
check "reload: the state is kept" "$(ask 'GET services\nColumns: state plugin_output')" "2;broken"
check "reload: the comments are kept" "$(ask 'GET comments\nColumns: comment\nFilter: entry_type = 1')" "kept over a reload on the host"
check "reload: the downtime is kept" "$(ask 'GET downtimes\nColumns: host_name comment')" "one;planned"
check "reload: the host is still in its downtime" "$(ask 'GET hosts\nColumns: scheduled_downtime_depth\nFilter: name = one')" "1"
check "reload: the same process" "$(kill -0 "$pid" 2>/dev/null && echo alive)" "alive"
if [ "$1" = sqlite ]; then
    check "reload: the log's earlier lines are there" "$(ask 'GET log\nStats: message ~ PASSIVE SERVICE CHECK: one;probe;2')" "1"
fi
command "PROCESS_SERVICE_CHECK_RESULT;one;probe;1;slow"
sleep 1.5
check "reload: the command file still takes commands" "$(ask 'GET services\nColumns: state plugin_output')" "1;slow"

# 3. A configuration that cannot be used.
{ base; service; second; echo 'define host {'; echo '    host_name three'; echo '}'; } > objects.cfg
kill -HUP "$pid"
wait_log "is not taken; the one that runs stays" || echo "FAIL  the broken configuration was not refused"
sleep 0.5
check "broken: the same process goes on" "$(kill -0 "$pid" 2>/dev/null && echo alive)" "alive"
check "broken: the hosts are as before" "$(ask 'GET hosts\nColumns: name')" "one two"
check "broken: the state is as before" "$(ask 'GET services\nColumns: state plugin_output')" "1;slow"

# 4. Mended, without the service, and RESTART_PROGRAM through Livestatus.
{ base; second; } > objects.cfg
printf 'COMMAND [%d] RESTART_PROGRAM\n\n' "$(date +%s)" | unixcat "$work/live"
wait_log "RESTART_PROGRAM, restarting" || echo "FAIL  no restart after RESTART_PROGRAM"
for _ in $(seq 50); do [ "$(ask 'GET services\nStats: state >= 0')" = "0" ] && break; sleep 0.2; done
check "restart: the service is gone" "$(ask 'GET services\nStats: state >= 0')" "0"
check "restart: its comment is gone, the host's is not" "$(ask 'GET comments\nColumns: comment\nFilter: entry_type = 1')" "on the host"
check "restart: the hosts are there" "$(ask 'GET hosts\nColumns: name')" "one two"
check "restart: the downtime is kept" "$(ask 'GET downtimes\nColumns: host_name comment')" "one;planned"

# 5. With a store: what commands changed outlasts a restart of the process.
if [ "$1" = sqlite ]; then
    command "DISABLE_HOST_FLAP_DETECTION;one"
    command "CHANGE_NORMAL_HOST_CHECK_INTERVAL;one;9"
    command "STOP_EXECUTING_SVC_CHECKS"
    sleep 1.5
    before=$(ask 'GET hosts\nColumns: flap_detection_enabled check_interval modified_attributes\nFilter: name = one')
    check "restart: set before it" "$before" "0;9.0000000000e+00;$(ask 'GET hosts\nColumns: modified_attributes\nFilter: name = one')"
    kill -TERM "$pid"
    wait "$pid"
    rm -f "$work/live"
    /checkd run --store "$store" --livestatus "$work/live" naemon.cfg >> /out/stdout.txt 2>&1 &
    pid=$!
    for _ in $(seq 50); do [ -S "$work/live" ] && break; sleep 0.2; done
    sleep 0.5
    check "restart: a host's settings are kept" "$(ask 'GET hosts\nColumns: flap_detection_enabled check_interval modified_attributes\nFilter: name = one')" "$before"
    check "restart: the program's switch is kept" "$(ask 'GET status\nColumns: execute_service_checks')" "0"
    check "restart: the other host is as configured" "$(ask 'GET hosts\nColumns: flap_detection_enabled modified_attributes\nFilter: name = two')" "1;0"
fi

kill -TERM "$pid"
wait "$pid"
check "ends cleanly" "$?" "0"
exit "$failures"
