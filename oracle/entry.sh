#!/bin/sh
# Runs inside the oracle image.
#   /in   the case directory, read-only
#   /out  where the results are written
# The case is copied to /case, so a case may use /case/... as absolute paths.
set -eu

main=${1:-naemon.cfg}
work=/tmp/oracle

cp -r /in/. /case/
mkdir -p "$work/checkresults"

# These three settings belong to the oracle: they point Naemon at paths that
# exist in the container. Any value a case gives for them is dropped.
owned='log_file|precached_object_file|check_result_path'
grep -v -E "^[[:space:]]*(${owned})[[:space:]]*=" "/in/$main" > "/case/$main" || true
cat >> "/case/$main" <<SETTINGS

log_file=$work/naemon.log
precached_object_file=$work/objects.precache
check_result_path=$work/checkresults
SETTINGS

rm -f /out/verify.txt /out/verify.exit /out/objects.precache /out/oracle.version

set +e
naemon -v -p "/case/$main" > /out/verify.txt 2>&1
code=$?
set -e
echo "$code" > /out/verify.exit

# The header of the precache file carries a timestamp, so it is left out.
if [ -f "$work/objects.precache" ]; then
    sed '/^#/d' "$work/objects.precache" > /out/objects.precache
fi

naemon --version | head -n 1 > /out/oracle.version
