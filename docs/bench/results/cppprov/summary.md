### Write to visible, ms (p50 / p95 / max over all reps)

| points | publish s | stack | seq p50 | seq p95 | seq max | sim:10 p50 | sim:10 p95 | burst p50 | burst p95 | burst max | seq applied p50 | burst applied p50 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 166 | 201 | 205 | 146 | 192 | 2606 | 4653 | 4729 | 49 | 2529 |
| 100 | 0.1 | ferrofire + stock FD | 99 | 101 | 115 | 84 | 112 | 2514 | 4605 | 4638 | 47 | 2513 |
| 100 | 0.1 | C++ provider + ferrofire FD | 150 | 201 | 204 | 143 | 186 | 131 | 189 | 190 | 49 | 36 |
| 100 | 0.1 | ferrofire + ferrofire FD | 99 | 101 | 104 | 89 | 144 | 66 | 81 | 125 | 47 | 27 |
| 100 | 0.05 | C++ provider + stock FD | 127 | 152 | 200 | 99 | 145 | 2606 | 4647 | 4696 | 49 | 2530 |
| 100 | 0.05 | ferrofire + stock FD | 49 | 99 | 103 | 49 | 98 | 2508 | 4556 | 4607 | 47 | 2505 |
| 100 | 0.05 | C++ provider + ferrofire FD | 148 | 152 | 156 | 93 | 143 | 88 | 138 | 138 | 50 | 34 |
| 100 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 101 | 41 | 72 | 52 | 70 | 80 | 47 | 51 |
| 1250 | 0.1 | C++ provider + stock FD | 103 | 205 | 208 | 124 | 193 | 5102 | 9627 | 9708 | 46 | 6005 |
| 1250 | 0.1 | ferrofire + stock FD | 99 | 102 | 105 | 87 | 94 | 5017 | 9517 | 9649 | 47 | 6015 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 103 | 205 | 207 | 87 | 189 | 165 | 207 | 210 | 45 | 59 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 99 | 102 | 127 | 90 | 94 | 89 | 142 | 147 | 46 | 44 |
| 1250 | 0.05 | C++ provider + stock FD | 151 | 157 | 163 | 124 | 144 | 5085 | 9587 | 9643 | 53 | 6019 |
| 1250 | 0.05 | ferrofire + stock FD | 49 | 98 | 102 | 50 | 78 | 5017 | 9519 | 9592 | 46 | 6015 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 118 | 159 | 169 | 99 | 149 | 91 | 141 | 144 | 49 | 33 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 49 | 99 | 103 | 35 | 66 | 54 | 98 | 103 | 47 | 51 |

### Burst: simultaneous writes, one Modbus client per RTU (10 RTUs), done within 10 s

| points | publish s | stack | burst size | issued | done | lost | lost % |
|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.1 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + stock FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | C++ provider + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 100 | 0.05 | ferrofire + ferrofire FD | 100 | 300 | 300 | 0 | 0.0 |
| 1250 | 0.1 | C++ provider + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | ferrofire + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 1000 | 3000 | 1415 | 1585 | 52.8 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |
| 1250 | 0.05 | C++ provider + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | ferrofire + stock FD | 1000 | 3000 | 600 | 2400 | 80.0 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 1000 | 3000 | 1520 | 1480 | 49.3 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 1000 | 3000 | 3000 | 0 | 0.0 |

### Publish rate and cost of the provider, idle (nothing driving it)

| points | publish s | stack | publishes/s | interval mean ms | p99 ms | datagrams per publish | bytes per datagram | provider cpu % of a core | provider peak RSS MB | 10 field devices cpu % | field device peak RSS MB (largest of the 10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 0.1 | C++ provider + stock FD | 9.73 | 103.1 | 110.4 | 2.0 | 1068 | 0.9 | 4.7 | 3.0 | 6.4 |
| 100 | 0.1 | ferrofire + stock FD | 10.05 | 100.0 | 108.6 | 1.0 | 277 | 1.2 | 7.1 | 2.1 | 6.3 |
| 100 | 0.1 | C++ provider + ferrofire FD | 9.48 | 106.0 | 110.6 | 2.0 | 1068 | 0.9 | 4.6 | 1.0 | 6.4 |
| 100 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.7 | 1.0 | 277 | 1.1 | 7.1 | 0.8 | 6.0 |
| 100 | 0.05 | C++ provider + stock FD | 18.98 | 52.8 | 60.3 | 2.0 | 1068 | 1.6 | 4.7 | 4.4 | 6.4 |
| 100 | 0.05 | ferrofire + stock FD | 20.05 | 50.0 | 58.7 | 1.0 | 277 | 1.9 | 7.1 | 2.8 | 6.3 |
| 100 | 0.05 | C++ provider + ferrofire FD | 18.12 | 55.4 | 60.5 | 2.0 | 1068 | 1.4 | 4.6 | 1.2 | 6.4 |
| 100 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.4 | 1.0 | 277 | 1.8 | 7.1 | 0.9 | 5.9 |
| 1250 | 0.1 | C++ provider + stock FD | 9.60 | 104.8 | 112.5 | 17.9 | 1475 | 5.1 | 5.2 | 10.2 | 7.5 |
| 1250 | 0.1 | ferrofire + stock FD | 10.05 | 100.0 | 109.0 | 3.0 | 943 | 3.2 | 7.8 | 2.9 | 7.5 |
| 1250 | 0.1 | C++ provider + ferrofire FD | 9.35 | 107.7 | 112.8 | 17.9 | 1475 | 4.6 | 5.6 | 3.3 | 6.7 |
| 1250 | 0.1 | ferrofire + ferrofire FD | 10.03 | 100.0 | 109.5 | 3.0 | 943 | 3.0 | 7.8 | 1.3 | 7.0 |
| 1250 | 0.05 | C++ provider + stock FD | 18.10 | 55.4 | 62.5 | 17.9 | 1475 | 9.1 | 5.3 | 17.7 | 7.5 |
| 1250 | 0.05 | ferrofire + stock FD | 20.05 | 50.0 | 59.0 | 3.0 | 943 | 5.5 | 7.8 | 4.0 | 7.5 |
| 1250 | 0.05 | C++ provider + ferrofire FD | 17.45 | 57.5 | 62.7 | 18.0 | 1475 | 8.0 | 5.6 | 5.6 | 7.0 |
| 1250 | 0.05 | ferrofire + ferrofire FD | 20.05 | 50.0 | 59.7 | 3.0 | 943 | 5.4 | 7.8 | 2.2 | 6.7 |
