# everything-burger switchboard `robust` (ebwes-robust)

- apps: `88643cc0ea` = main `88643cc0ea` + no PRs
- bennu: #81 `64e4bb5d9c` deb `6.0.1794326112.64e4bb5` wheel `None`
- topologies: `site route` (linux-only)
- synced 2026-11-10T15:55:01+00:00

| item | check | verdict | reason | paths |
|---|---|---|---|---|
| - | phenix-apps install | ok |  |  |
| - | experiment + VMs running | ok |  |  |
| - | phenix-scorch-validate | ok |  | `everything-burger-with-extra-sauce/work/robust/out/models.yaml` |
| health | status start | ok |  | `everything-burger-with-extra-sauce/work/robust/results/run-0/health` |
| modbus-badframes | status start | ok |  | `everything-burger-with-extra-sauce/work/robust/results/run-1/modbus-badframes` |
| iec104-masters | status start | ok |  | `everything-burger-with-extra-sauce/work/robust/results/run-1/iec104-masters` |
| dnp3-accept | status start | ok |  | `everything-burger-with-extra-sauce/work/robust/results/run-1/dnp3-accept` |
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
| modbus-badframes | python | ok | malformed frames dropped, same pid, normal read answers (4 cases; log grew 1470 B) | `everything-burger-with-extra-sauce/work/robust/results/run-1/modbus-badframes/loop-0-count-0` |
| iec104-masters | python | ok | A active, receiving; A still receiving with B connected; A still receiving after B closed; C active after A closed, receiving | `everything-burger-with-extra-sauce/work/robust/results/run-1/iec104-masters/loop-0-count-0` |
| dnp3-accept | python | ok | outstation answers a link status request; fd limit 9 then restored; outstation resumed accepting and answers | `everything-burger-with-extra-sauce/work/robust/results/run-1/dnp3-accept/loop-0-count-0` |
| - | field-device logs | WARN | 0 crash-class line(s); 9 error-class line(s); flood on rtu-1, rtu-2 | `everything-burger-with-extra-sauce/work/robust/results/fdlogs` |

## Field-device logs

fail_on: `crash`; flood: over 50 MB/min or one line more than 5000x. Only the last 2 MB of each file is scanned; sizes are the whole file.

| host | files | sizes | crash | error | flood | top lines |
|---|---|---|---|---|---|---|
| rtu-1 | 5 | default-bennu-field-deviced-stdout.log 108, default-bennu-field-deviced-stderr.log 929, rtu-1-modbus-tcp-server.log 27,394, watcher-stdout.log 496, watcher-stderr.log 173,512 | 0 | 9 | watcher-stderr.log: one line repeated 17340x: pid = N | 3x `Modbus TCP receive message of length 0 failed with error: Message too long`<br>3x `136,2026-Nov-10-16:00:01,error,modbus-server,read holding registers,Invalid read`<br>2x `Modbus RTU receive message failed with error: End of file` |
| rtu-2 | 5 | default-bennu-field-deviced-stdout.log 66, default-bennu-field-deviced-stderr.log 301, rtu-2-modbus-tcp-server.log 28,051, watcher-stdout.log 496, watcher-stderr.log 181,542 | 0 | 0 | watcher-stderr.log: one line repeated 18143x: pid = N | - |

**PASS**
