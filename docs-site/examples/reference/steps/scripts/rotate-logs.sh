#!/bin/sh
# Compresses each log file in a directory and starts a new, empty one.
set -eu
stamp=$(date +%Y%m%d)
for log in "$1"/*.log; do
    gzip -c "$log" >"$log.$stamp.gz"
    : >"$log"
done
