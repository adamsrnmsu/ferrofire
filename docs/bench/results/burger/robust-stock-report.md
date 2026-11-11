# everything-burger switchboard `robstock` (ebwes-robstock)

- apps: `88643cc0ea` = main `88643cc0ea` + no PRs
- bennu: stock bennu.qc2
- topologies: `site route` (linux-only)
- synced 2026-11-10T14:22:32+00:00

| item | check | verdict | reason | paths |
|---|---|---|---|---|
| - | phenix-apps install | ok |  |  |
| - | experiment + VMs running | ok |  |  |
| - | phenix-scorch-validate | ok |  | `everything-burger-with-extra-sauce/work/robstock/out/models.yaml` |
| health | status start | ok |  | `everything-burger-with-extra-sauce/work/robstock/results/run-0/health` |
| modbus-badframes | status start | ok |  | `everything-burger-with-extra-sauce/work/robstock/results/run-1/modbus-badframes` |
| iec104-masters | status start | ok |  | `everything-burger-with-extra-sauce/work/robstock/results/run-1/iec104-masters` |
| dnp3-accept | status start | ok |  | `everything-burger-with-extra-sauce/work/robstock/results/run-1/dnp3-accept` |
| field-device | vm rtu-1: ss -Hltn '( sport = :502 )' / grep -q . | ok |  |  |
| field-device | vm rtu-2: ss -Hltn '( sport = :502 )' / grep -q . | ok |  |  |
| field-device | vm rtu-1: pgrep -f '(^//)bennu-field-deviced?( /$)' | ok |  |  |
| field-device | vm rtu-2: pgrep -f '(^//)bennu-field-deviced?( /$)' | ok |  |  |
| field-device | vm rtu-1: ep=$(sed -n 's#.*<endpoint>\(tcp://[^<]*\)</endpoint>.*#\1#p | ok |  |  |
| field-device | vm rtu-2: ep=$(sed -n 's#.*<endpoint>\(tcp://[^<]*\)</endpoint>.*#\1#p | ok |  |  |
| simulink-provider | vm waterway-provider: pgrep -f '(^//)bennu-simulink-provider(-helics)?( /$)' | ok |  |  |
| simulink-provider | vm waterway-provider: pgrep -f '(^//)simulinksolver( /$)' | ok |  |  |
| modbus-badframes | python | FAIL | answered a malformed frame: truncated request, MBAP length 0; field device pid 434 -> None; the server no longer answers a normal Modbus read (4 cases; log grew 6396 B) | `everything-burger-with-extra-sauce/work/robstock/results/run-1/modbus-badframes/loop-0-count-0` |
| iec104-masters | python | FAIL | RuntimeError: A stopped receiving after a second client connected (after: A active, receiving) | `everything-burger-with-extra-sauce/work/robstock/results/run-1/iec104-masters/loop-0-count-0` |
| dnp3-accept | python | FAIL | RuntimeError: outstation did not resume accepting after an accept() error (after: outstation answers a link status request; fd limit 11 then restored) | `everything-burger-with-extra-sauce/work/robstock/results/run-1/dnp3-accept/loop-0-count-0` |

**FAIL**: 3 failing check(s)
