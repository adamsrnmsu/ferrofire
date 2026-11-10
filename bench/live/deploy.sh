#!/bin/bash
# deploy.sh file...: push bench/live files to /root/ff on provider and RTUs through minimega cc (the range has no host route; run from the Mac, needs the Lima VM)
for f in "$@"; do limactl shell phenix sudo docker cp "$(cd "$(dirname "$0")" && pwd)"/$f minimega:/phenix/images/ffdeploy/$f; done
"$(dirname "$0")"/rng.sh all 30 "cd /tmp/miniccc/files/ffdeploy 2>/dev/null && rm -f $*" >/dev/null
for f in "$@"; do limactl shell phenix sudo docker exec minimega mm namespace waterway cc send ffdeploy/$f 2>&1 | grep -v INFO; done
sleep 12
"$(dirname "$0")"/rng.sh all 30 'case $(hostname) in waterway-provider|rtu-1|rtu-2) cd /root/ff; for f in '"$*"'; do cp /tmp/miniccc/files/ffdeploy/$f /root/ff/$f; done; chmod +x *.sh *.py; md5sum '"$*"' | cut -c1-8 | tr "\n" " ";; esac' | grep -E "^[0-9a-f]{8}"
