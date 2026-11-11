#!/bin/sh
# Runs inside a node's container: checkd in failover, started again
# whenever it ends with another status than 0, as a supervisor such as
# systemd would: 3 when it lost the lease, 2 when it could not start, for
# example while the database cannot be reached.
#   /in      the case, read-only
#   /shared  the scripted plugins, read-only
#   /checkd  the checkd program
#   /case/state  the plugins' state and trace, kept outside the container
#   /out     the node's log
#   $1 this node's name, $2 the other node's name
#   START    when the run began, in ns, so both nodes' plugins agree
set -u
work=/tmp/oracle
mkdir -p /case/plugins "$work/checkresults"
cp -r /in/. /case/
cp /shared/plugins/* /case/plugins/
[ -f /case/state/start ] || echo "$START" > /case/state/start
cat >> /case/naemon.cfg <<SETTINGS

log_file=/out/$1.log
command_file=$work/checkd.cmd
check_result_path=$work/checkresults
state_retention_file=$work/retention.dat
broker_module=/usr/lib/naemon/naemon-livestatus/livestatus.so $work/live
SETTINGS
while true; do
    /checkd run --failover "$1" --peer "$2:6558" --lease 15 \
        --store "postgres:host=pg user=checkd dbname=checkd sslmode=disable" \
        /case/naemon.cfg >> "/out/$1.stdout" 2>&1
    code=$?
    echo "[$(date +%s)] checkd ended with status $code" >> "/out/$1.log"
    [ "$code" = 0 ] && break
    sleep 2
done
