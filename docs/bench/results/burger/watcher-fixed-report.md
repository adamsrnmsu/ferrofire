# everything-burger switchboard `watcher` (ebwes-watcher)

- apps: `88643cc0ea` = main `88643cc0ea` + no PRs
- bennu: fork/fix-watcher-log-flood `400b48c2c5` deb `6.0.1794340973.400b48c` wheel `None`
- topologies: `site route`
- synced 2026-11-10T20:02:42+00:00

| item | check | verdict | reason | paths |
|---|---|---|---|---|
| - | phenix-apps install | ok |  |  |
| - | experiment + VMs running | ok |  |  |
| - | phenix-scorch-validate | ok |  | `everything-burger-with-extra-sauce/work/watcher/out/models.yaml` |
| health | status start | ok |  | `everything-burger-with-extra-sauce/work/watcher/results/run-0/health` |
| watcher-quiet | status start | ok |  | `everything-burger-with-extra-sauce/work/watcher/results/run-1/watcher-quiet` |
| bennu | vm rtu-1: cat /var/log/burger-bennu.log 2>&1 | ok |  |  |
| bennu | vm rtu-1: dpkg-query -W bennu 2>/dev/null / cut -f2 / grep . // echo n | ok |  |  |
| bennu | vm rtu-2: cat /var/log/burger-bennu.log 2>&1 | ok |  |  |
| bennu | vm rtu-2: dpkg-query -W bennu 2>/dev/null / cut -f2 / grep . // echo n | ok |  |  |
| bennu | vm waterway-provider: cat /var/log/burger-bennu.log 2>&1 | ok |  |  |
| bennu | vm waterway-provider: dpkg-query -W bennu 2>/dev/null / cut -f2 / grep . // echo n | ok |  |  |
| field-device | vm rtu-1: ss -Hltn '( sport = :502 )' / grep -q . | ok |  |  |
| field-device | vm rtu-2: ss -Hltn '( sport = :502 )' / grep -q . | ok |  |  |
| field-device | vm rtu-1: pgrep -f '(^//)bennu-field-deviced?( /$)' | ok |  |  |
| field-device | vm rtu-2: pgrep -f '(^//)bennu-field-deviced?( /$)' | ok |  |  |
| field-device | vm rtu-1: ep=$(sed -n 's#.*<endpoint>\(tcp://[^<]*\)</endpoint>.*#\1#p | ok |  |  |
| field-device | vm rtu-2: ep=$(sed -n 's#.*<endpoint>\(tcp://[^<]*\)</endpoint>.*#\1#p | ok |  |  |
| simulink-provider | vm waterway-provider: pgrep -f '(^//)bennu-simulink-provider(-helics)?( /$)' | ok |  |  |
| simulink-provider | vm waterway-provider: pgrep -f '(^//)simulinksolver( /$)' | ok |  |  |
| modbus-polled | vm rtu-1: ss -Htn state established '( sport = :502 )' / grep -q . | ok |  |  |
| modbus-polled | vm rtu-2: ss -Htn state established '( sport = :502 )' / grep -q . | ok |  |  |
| watcher-quiet | python | ok | watcher-stderr.log quiet (rtu-1 0 B/min, rtu-2 0 B/min) | `everything-burger-with-extra-sauce/work/watcher/results/run-1/watcher-quiet/loop-0-count-0` |
| - | field-device logs | ok | 0 crash-class line(s); 0 error-class line(s) | `everything-burger-with-extra-sauce/work/watcher/results/fdlogs` |

## Field-device logs

fail_on: `crash`; flood: over 50 MB/min or one line more than 5000x. Only the last 2 MB of each file is scanned; sizes are the whole file.

| host | files | sizes | crash | error | flood | top lines |
|---|---|---|---|---|---|---|
| rtu-1 | 5 | default-bennu-field-deviced-stdout.log 66, default-bennu-field-deviced-stderr.log 301, rtu-1-modbus-tcp-server.log 43,240, watcher-stdout.log 496, watcher-stderr.log 102 | 0 | 0 | no | - |
| rtu-2 | 5 | default-bennu-field-deviced-stdout.log 66, default-bennu-field-deviced-stderr.log 301, rtu-2-modbus-tcp-server.log 43,845, watcher-stdout.log 496, watcher-stderr.log 102 | 0 | 0 | no | - |

**PASS**
