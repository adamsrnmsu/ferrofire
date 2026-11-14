### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 101 | 201 | 202 | 128 | 185 | 137 | 180 | 181 | 44 | 32 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 99 | 100 | 101 | 108 | 119 | 107 | 129 | 129 | 47 | 55 |
| 100 | 0.1 | C++ provider + ferrofire FD | 102 | 201 | 208 | 113 | 170 | 108 | 200 | 201 | 46 | 55 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 102 | 151 | 160 | 108 | 147 | 89 | 142 | 143 | 45 | 32 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 49 | 99 | 101 | 53 | 86 | 55 | 82 | 83 | 47 | 53 |
| 100 | 0.05 | C++ provider + ferrofire FD | 148 | 153 | 159 | 129 | 157 | 99 | 168 | 169 | 52 | 41 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 103 | 203 | 206 | 135 | 200 | 168 | 189 | 194 | 47 | 63 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 99 | 102 | 107 | 111 | 130 | 96 | 114 | 117 | 47 | 48 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 199 | 203 | 207 | 109 | 113 | 132 | 189 | 191 | 51 | 61 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 149 | 155 | 160 | 114 | 144 | 121 | 154 | 159 | 51 | 55 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 49 | 99 | 101 | 64 | 78 | 48 | 91 | 101 | 47 | 45 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 150 | 155 | 204 | 101 | 144 | 114 | 129 | 162 | 51 | 44 |

### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |

### Flaps: the read-back showed the new state, then the old, then the new again (all cells pooled)

| points | publish s | stack | commands | flaps | flaps % |
|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 510 | 267 | 52.4 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 510 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 510 | 490 | 96.1 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 510 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 3510 | 2161 | 61.6 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 3510 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 3510 | 3493 | 99.5 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 3510 | 104 | 3.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 3510 | 0 | 0.0 |

### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + bennu v2026.11.03 FD | 9.72 | 103.5 | 110.2 | 2.0 | 1068 | 0.9 | 5.3 | 3.0 | 7.0 |
| 100 | 0.1 | ferrofire + bennu v2026.11.03 FD | 10.05 | 100.0 | 109.2 | 1.0 | 277 | 1.2 | 7.1 | 2.2 | 6.9 |
| 100 | 0.1 | C++ provider + ferrofire FD | 9.48 | 105.9 | 110.5 | 2.0 | 1068 | 0.8 | 5.3 | 0.9 | 6.6 |
| 100 | 0.05 | C++ provider + bennu v2026.11.03 FD | 18.95 | 52.9 | 60.2 | 2.0 | 1068 | 1.5 | 5.3 | 4.4 | 7.0 |
| 100 | 0.05 | ferrofire + bennu v2026.11.03 FD | 20.05 | 50.0 | 59.0 | 1.0 | 277 | 1.9 | 7.1 | 2.7 | 6.9 |
| 100 | 0.05 | C++ provider + ferrofire FD | 18.15 | 55.3 | 60.4 | 2.0 | 1068 | 1.3 | 5.3 | 1.3 | 6.5 |
| 1250 | 0.1 | C++ provider + bennu v2026.11.03 FD | 9.65 | 104.3 | 111.4 | 18.9 | 1399 | 3.6 | 5.8 | 9.0 | 8.1 |
| 1250 | 0.1 | ferrofire + bennu v2026.11.03 FD | 10.05 | 100.0 | 109.5 | 3.0 | 943 | 3.1 | 7.8 | 2.7 | 8.1 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 9.42 | 106.8 | 111.8 | 18.9 | 1399 | 3.5 | 5.8 | 3.5 | 7.3 |
| 1250 | 0.05 | C++ provider + bennu v2026.11.03 FD | 18.53 | 54.1 | 61.5 | 19.0 | 1399 | 6.6 | 5.8 | 15.9 | 8.1 |
| 1250 | 0.05 | ferrofire + bennu v2026.11.03 FD | 20.03 | 50.0 | 58.9 | 3.0 | 943 | 5.4 | 7.9 | 3.9 | 8.1 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 17.88 | 56.0 | 61.7 | 18.9 | 1399 | 6.0 | 5.8 | 6.2 | 6.9 |
