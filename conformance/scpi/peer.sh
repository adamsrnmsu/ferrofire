#!/bin/bash
# shellcheck shell=bash disable=SC2154
# Stand-in provider and one datagram of values, then PyVISA, lxi-tools, ferrofire scpi --check, a churn of 2000 connections watched
# through the device's descriptor count, and overlong and garbage lines. Ends with a plain-socket transcript that check.sh finds in the
# capture.
d=$(dirname "$0")
python3 "$d/provider.py" /tmp/provider.log &
python3 - <<'PY' &
import socket, time
g = b"54386972605942b"
body = b"load-1_bus-1.mw:250.5,line-1.mw:-12.25,generator-1_bus-1.active:true,\0"
u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
u.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
for _ in range(240):   # keep telling the device the values for the whole run
    u.sendto(bytes([len(g)]) + g + body, ("239.0.0.1", 40000)); time.sleep(0.5)
PY
for _ in $(seq 60); do python3 -c 'import socket; socket.create_connection(("127.0.0.1", 5025), 1)' 2>/dev/null && break; sleep 0.5; done
for _ in $(seq 60); do [ "$(ferrofire scpi 127.0.0.1 'MEAS:ANAL:LEVEL?' 2>/dev/null)" = 250.5 ] && break; sleep 0.5; done
echo "== PyVISA (pyvisa-py, TCPIP::127.0.0.1::5025::SOCKET)"
/opt/pyvisa/bin/python "$d/pyvisa_client.py" /tmp/provider.log || exit 1
echo "== lxi-tools: lxi scpi --raw"
idn=$(lxi scpi --raw -a 127.0.0.1 -p 5025 '*IDN?') || { echo "FAIL: lxi scpi *IDN? failed"; exit 1; }
echo "lxi *IDN? -> $idn"
case "$idn" in ferrofire,rtu-1,CONF-0001,*) echo "PASS lxi: *IDN?" ;; *) echo "FAIL lxi: *IDN? -> $idn"; exit 1 ;; esac
lvl=$(lxi scpi --raw -a 127.0.0.1 -p 5025 'MEAS:ANAL:LEVEL?')
echo "lxi MEAS:ANAL:LEVEL? -> $lvl"
case "$lvl" in 250.5*) echo "PASS lxi: MEAS:ANAL:LEVEL? = 250.5" ;; *) echo "FAIL lxi: level -> $lvl"; exit 1 ;; esac
echo "== ferrofire scpi --check"
cat > /tmp/script.scpi <<'S'
# a script for ferrofire scpi --check
*CLS
*IDN?
MEAS:ANAL:FLOW?
SOUR:SETPOINT 300
SOUR:BREAKER_CMD 0
MEAS:BIN:BRKR?;:MEAS:ANAL:LEVEL?
S
ferrofire scpi 127.0.0.1 --check --file /tmp/script.scpi && echo "PASS ferrofire scpi: --check script exit 0" || { echo "FAIL ferrofire scpi: --check script"; exit 1; }
ferrofire scpi 127.0.0.1 --check --json '*OPC?' 'MEAS:ANAL:LEVEL?'
ferrofire scpi 127.0.0.1 --check --timeout 1 'SOUR:SETPOINT 99999' 2> /tmp/check-err.txt; rc=$?
cat /tmp/check-err.txt
{ [ "$rc" = 1 ] && grep -q -- '-222,' /tmp/check-err.txt; } && echo "PASS ferrofire scpi: --check exits 1 with the -222 text" || { echo "FAIL ferrofire scpi: --check on a bad write (rc=$rc)"; exit 1; }
sleep 1
echo "== provider log"; cat /tmp/provider.log
echo "== churn: 2000 connections, the device's descriptors must stay flat"
pid=$(pgrep -f 'ferrofire field-device' | head -1)
fds() { ls "/proc/$pid/fd" | wc -l; }
before=$(fds); echo "device pid $pid fds before: $before"
python3 - <<'PY'
import socket, struct, random
random.seed(7)
for i in range(2000):
    s = socket.create_connection(("127.0.0.1", 5025), 5)
    k = i % 5
    if k == 0:
        s.sendall(b"*OPC?\n"); assert s.recv(16) == b"1\n"
    elif k == 1:
        s.sendall(b"*IDN?\nSOUR:SETPOINT")             # a half-sent line, then close
    elif k == 2:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))   # abort with a RST
    elif k == 3:
        s.sendall(b"*OPC?\n"); s.shutdown(socket.SHUT_WR); assert s.recv(16) == b"1\n"
    s.close()
print("churn done: 2000 connections")
PY
sleep 3
after=$(fds); echo "device fds after: $after"
[ "$after" -le $((before + 3)) ] && echo "PASS churn: descriptors flat ($before -> $after)" || { echo "FAIL churn: descriptors $before -> $after"; exit 1; }
echo "== overlong and garbage lines"
python3 - <<'PY'
import socket, os, time
def ask(s, line):
    s.sendall(line + b"\n"); return s.recv(4096).decode().strip()
s = socket.create_connection(("127.0.0.1", 5025), 5); s.settimeout(5)
s.sendall(b"A" * 200_000 + b"\n")                       # over max-line (1024): -363, thrown away to the newline
print("overlong line:", ask(s, b"SYST:ERR?"))
assert ask(s, b"SYST:ERR?").startswith("0,"), "one error for the whole overlong line"
s.sendall(os.urandom(65536).replace(b"\n", b"x") + b"\n")  # random bytes, no newline until the end
s.sendall(b"\x00\xff\xfe;;;:::\"\"\"'''\n")
print("after garbage, *IDN? ->", ask(s, b"*IDN?"))
assert ask(s, b"*OPC?") == "1"
print("garbage errors queued:", ask(s, b"SYST:ERR:COUN?"))
print("PASS plain socket: overlong and garbage lines handled, the connection still answers")
s.close()
s = socket.create_connection(("127.0.0.1", 5025), 5); s.settimeout(5)
s.sendall(b"*IDN?\r\n"); print("CRLF *IDN? ->", s.recv(4096).decode().strip())
s.close()
PY
[ $? = 0 ] || { echo "FAIL: overlong or garbage line handling"; exit 1; }
echo "== the device is still serving"
ferrofire scpi 127.0.0.1 '*IDN?'
echo "SCPI PEERS DONE"
