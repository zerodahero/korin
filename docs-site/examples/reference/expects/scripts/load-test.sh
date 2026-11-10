#!/bin/sh
# Runs a short load test against the staging site.
set -eu
exec hey -z 30s https://staging.shop.example.com/
