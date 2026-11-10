#!/bin/sh
# Prints the size of each log file in a directory.
set -eu
du -sh "$1"/*.log
