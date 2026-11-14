#!/bin/bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# lines.sh W NAME BINARY SYM [N]
docker run --rm -v /var/tmp/ff-prof:/out -v /Users/ryanadams/git/ferrofire:/src ffprof-perf:22.04 bash -c "P=\$(ls /usr/lib/linux-tools/*/perf); ln -sf \$P /usr/local/bin/perf; python3 /out/scripts/lines.py /out/$1/$2.perf.data /src/target-prof/release/$3 '$4' $5 ${6:-10}"
