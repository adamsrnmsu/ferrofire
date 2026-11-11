# everything-burger switchboard `wstock` (ebwes-wstock)

- apps: `88643cc0ea` = main `88643cc0ea` + no PRs
- bennu: stock bennu.qc2
- topologies: `site route`
- synced 2026-11-10T19:48:57+00:00

| item | check | verdict | reason | paths |
|---|---|---|---|---|
| - | phenix-apps install | ok |  |  |
| - | experiment + VMs running | ok |  |  |
| - | phenix-scorch-validate | ok |  | `everything-burger-with-extra-sauce/work/wstock/out/models.yaml` |
| health | status start | ok |  | `everything-burger-with-extra-sauce/work/wstock/results/run-0/health` |
| watcher-quiet | status start | ok |  | `everything-burger-with-extra-sauce/work/wstock/results/run-1/watcher-quiet` |
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
| watcher-quiet | python | FAIL | rtu-1: watcher-stderr.log grew 57780 B/min, over 2048; rtu-2: watcher-stderr.log grew 57723 B/min, over 2048 (rtu-1 57780 B/min, rtu-2 57723 B/min) | `everything-burger-with-extra-sauce/work/wstock/results/run-1/watcher-quiet/loop-0-count-0` |
| - | field-device logs | WARN | 0 crash-class line(s); 0 error-class line(s); flood on rtu-1, rtu-2 | `everything-burger-with-extra-sauce/work/wstock/results/fdlogs` |

## Field-device logs

fail_on: `none`; flood: over 50 MB/min or one line more than 5000x. Only the last 2 MB of each file is scanned; sizes are the whole file.

| host | files | sizes | crash | error | flood | top lines |
|---|---|---|---|---|---|---|
| rtu-1 | 5 | default-bennu-field-deviced-stdout.log 167,998, default-bennu-field-deviced-stderr.log 301, rtu-1-modbus-tcp-server.log 35,590, watcher-stdout.log 496, watcher-stderr.log 229,692 | 0 | 0 | watcher-stderr.log: one line repeated 22958x: pid = N | - |
| rtu-2 | 5 | default-bennu-field-deviced-stdout.log 184,108, default-bennu-field-deviced-stderr.log 301, rtu-2-modbus-tcp-server.log 36,544, watcher-stdout.log 496, watcher-stderr.log 237,722 | 0 | 0 | watcher-stderr.log: one line repeated 23761x: pid = N | - |

**FAIL**: 1 failing check(s)
