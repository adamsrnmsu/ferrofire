#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# alt.sh WORKLOAD VARIANT [N=3] : alternate base/variant N times
W=$1; V=$2; N=${3:-3}
for i in $(seq $N); do /var/tmp/ff-prof/scripts/mm.sh $W base; /var/tmp/ff-prof/scripts/mm.sh $W $V; done
