#!/bin/bash
# cap.sh NAME HOST SECS CMD... : run CMD on HOST (through on.sh, stderr merged) and save "$ CMD" plus its output to captures/NAME.txt
d="$(cd "$(dirname "$0")" && pwd)"; n=$1; h=$2; s=$3; shift 3
{ echo "# $h, $(date -u +%FT%TZ)"; echo "\$ $*"; "$d/on.sh" "$h" "$s" "exec 2>&1; $*"; } | tee "$d/captures/$n.txt"
