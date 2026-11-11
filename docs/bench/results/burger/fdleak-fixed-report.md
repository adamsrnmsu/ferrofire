# everything-burger switchboard `fdleak` (ebwes-fdleak)

- apps: `88643cc0ea` = main `88643cc0ea` + no PRs
- bennu: #79 `afe6ccc841` deb `6.0.1794318367.afe6ccc` wheel `None`
- topologies: `site route` (linux-only)
- synced 2026-11-10T13:45:56+00:00

| item | check | verdict | reason | paths |
|---|---|---|---|---|
| - | phenix-apps install | ok |  |  |
| - | experiment + VMs running | ok |  |  |
| - | phenix-scorch-validate | ok |  | `everything-burger-with-extra-sauce/work/fdleak/out/models.yaml` |
| health | status start | ok |  | `everything-burger-with-extra-sauce/work/fdleak/results/run-0/health` |
| modbus-churn | status start | ok |  | `everything-burger-with-extra-sauce/work/fdleak/results/run-1/modbus-churn` |
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
| modbus-churn | python | ok | no fd leak, server still answers (1500/1500 connections; socket fds 5 -> 5; log grew 54620 B) | `everything-burger-with-extra-sauce/work/fdleak/results/run-1/modbus-churn/loop-0-count-0` |

**PASS**
