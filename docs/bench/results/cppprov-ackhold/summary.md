### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 148 | 152 | 153 | 118 | 142 | 106 | 130 | 156 | 50 | 46 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 49 | 97 | 102 | 42 | 93 | 48 | 97 | 99 | 47 | 46 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 143 | 154 | 160 | 114 | 133 | 118 | 142 | 168 | 50 | 51 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 49 | 98 | 101 | 34 | 87 | 30 | 77 | 81 | 47 | 28 |

### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 1000 | 3000 | 3000 | 0 | 0.0 |

### Flaps: the read-back showed the new state, then the old, then the new again (all cells pooled)

| points | publish s | stack | commands | flaps | flaps % |
|---|---|---|---|---|---|
| 100 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 510 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 510 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 3510 | 0 | 0.0 |

### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 19.05 | 52.7 | 60.1 | 2.0 | 1068 | 1.4 | 5.3 | 4.5 | 7.1 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 20.03 | 50.0 | 58.6 | 1.0 | 277 | 1.7 | 6.9 | 2.9 | 7.1 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 + ACK hold FD | 18.73 | 53.6 | 61.0 | 19.0 | 1399 | 6.4 | 5.8 | 16.1 | 8.2 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 + ACK hold FD | 20.05 | 50.0 | 59.0 | 3.0 | 943 | 3.9 | 7.8 | 4.1 | 8.2 |
