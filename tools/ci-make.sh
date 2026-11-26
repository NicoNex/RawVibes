#!/bin/sh
# make with the given arguments; on failure, the log's last 40 lines become a GitHub annotation,
# readable without opening the run.
set -o pipefail 2>/dev/null || true
make "$@" 2>&1 | tee build.log
status=$?
if [ "$status" -ne 0 ]; then
  echo "::error title=make $*::$(tail -40 build.log | awk '{gsub(/%/,"%25"); printf "%s%%0A", $0}')"
fi
exit "$status"
