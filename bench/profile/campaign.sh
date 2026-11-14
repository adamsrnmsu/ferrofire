#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
S=/var/tmp/ff-prof/scripts
for spec in "big A" "query E" "gridfast-idle C" "gridfast-idle D" "m800 C" "grid-storm B" "grid-storm F" "gridfast-storm B" "gridfast-storm F" "gridfast-storm C"; do
  set -- $spec; echo "#### $spec"; $S/alt.sh $1 $2 3
done
echo CAMPAIGNDONE
