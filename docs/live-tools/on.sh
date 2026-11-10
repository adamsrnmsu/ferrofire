#!/bin/bash
# on.sh HOST SECS CMD... : run CMD on a range VM through minimega cc (bench/live/rng.sh) and print just its output
d="$(cd "$(dirname "$0")/../../bench/live" && pwd)"
"$d/rng.sh" "$@" | grep -v -E '^(lima-phenix: )?[0-9]+/[0-9a-f-]+/std(out|err):$' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}'
