#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# post.sh WORKLOAD NAME BINARY [N] : demangle, top table -> W/NAME.top.md, flamegraph -> W/NAME.svg  (host)
cd /var/tmp/ff-prof
docker run --rm -v /var/tmp/ff-prof:/out -v /Users/ryanadams/git/ferrofire:/src ffprof-perf:24.04 bash -c "python3 /out/scripts/top.py /out/$1/$2.folded /src/target-prof/release/$3 ${4:-15} > /out/$1/$2.top.md; /opt/FlameGraph/flamegraph.pl --title '$1 $2' --width 1600 /out/$1/$2.dem.folded > /out/$1/$2.svg"
cat $1/$2.top.md
