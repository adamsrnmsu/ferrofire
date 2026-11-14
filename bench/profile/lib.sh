#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2034,SC2069,SC2120,SC2164  # run-once record of the ww-1df.34 profiling, paths are the scratch dir
# source me. Host-side helpers; repo at /Users/ryanadams/git/ferrofire.
R=/Users/ryanadams/git/ferrofire
FF=/src/target-prof/release/ferrofire
NET=ffprof
LBL="--label ffprof=1"
MNT="-v $R:/src -v /var/tmp/ff-prof:/out"
cleanup() { docker ps -aq --filter label=ffprof=1 | xargs -r docker rm -f >/dev/null 2>&1; }
guard() { # refuse to start while another cargo/bench container (not ours, not the stale one) runs
  docker ps --format '{{.Names}} {{.Image}} {{.Command}}' | grep -v -e elegant_zhukovsky -e ffprof-build -e '^ffp-' | grep -E 'ff-rust|cargo|ffbench' && { echo "contention, abort"; return 1; }; return 0; }
dr() { docker run -d $LBL --platform linux/arm64 --network $NET $MNT "$@"; }
pid() { docker inspect -f '{{.State.Pid}}' "$1"; }
# perf W SECS name=container ...
perfrec() { W=$1; S=$2; shift 2; args=""; for a in "$@"; do args="$args ${a%%=*}=$(pid ${a##*=})"; done
  docker run --rm --privileged --pid=host --platform linux/arm64 -v /var/tmp/ff-prof:/out -v $R:/src ffprof-perf:22.04 /out/scripts/rec.sh $W $S $args; }
