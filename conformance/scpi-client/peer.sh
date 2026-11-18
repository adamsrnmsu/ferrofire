#!/bin/bash
# shellcheck shell=bash
# The independent instrument (scpi-parser-tcp) comes up after the controller, which has been retrying with backoff. The harness then sets
# `want` on the controller's own server; <logic> turns it into `*ESE <v>` for the instrument, the controller reads `*ESE?` back into `ese`,
# and the harness reads `ese` from the controller: two values, so neither can be an initial state.
# The VXI-11 twin (scpi-parser-vxi11) registers its three RPC programs with the portmapper, so rpcbind comes first.
rpcbind
for _ in $(seq 20); do rpcinfo -p 127.0.0.1 >/dev/null 2>&1 && break; sleep 0.25; done
scpi-parser-tcp > /tmp/instrument.out 2>&1 &
scpi-parser-vxi11 > /tmp/instrument-vxi11.out 2>&1 &
ctl() { ferrofire scpi 127.0.0.1:5030 "$@" 2>/dev/null; }
for _ in $(seq 60); do ctl '*OPC?' >/dev/null && break; sleep 0.5; done
for v in 36 4; do
  ctl "SOUR:WANT $v"
  got=
  for _ in $(seq 120); do got=$(ctl 'MEAS:SCAL:ESE?'); [ "$got" = "$v" ] && break; sleep 0.5; done
  echo "controller ese after want=$v: $got"
  [ "$got" = "$v" ] && echo "PASS scpi-parser: *ESE $v set by the controller and read back" || { echo "FAIL scpi-parser: *ESE $v (ese=$got)"; exit 1; }
done
# the same two values through the VXI-11 instrument (the logic sent them to both, `vese` is read back from it)
for v in 36 4; do
  ctl "SOUR:WANT $v"
  got=
  for _ in $(seq 120); do got=$(ctl 'MEAS:SCAL:VESE?'); [ "$got" = "$v" ] && break; sleep 0.5; done
  echo "controller vese after want=$v: $got"
  [ "$got" = "$v" ] && echo "PASS scpi-parser-vxi11: *ESE $v set by the controller and read back" || { echo "FAIL scpi-parser-vxi11: *ESE $v (vese=$got)"; exit 1; }
done
vvolt=$(ctl 'MEAS:SCAL:VVOLT?')
echo "controller vvolt (MEAS:VOLT:DC? of the VXI-11 instrument): $vvolt"
[ "$vvolt" = 0 ] && echo "PASS scpi-parser-vxi11: MEAS:VOLT:DC? answer taken as a number" || { echo "FAIL scpi-parser-vxi11: vvolt=$vvolt"; exit 1; }
volt=$(ctl 'MEAS:SCAL:VOLT?')
echo "controller volt (MEAS:VOLT:DC? of the instrument): $volt"
[ "$volt" = 0 ] && echo "PASS scpi-parser: MEAS:VOLT:DC? answer taken as a number" || { echo "FAIL scpi-parser: volt=$volt"; exit 1; }
echo "== instrument stdout"; cat /tmp/instrument.out
echo "== vxi11 instrument stdout"; cat /tmp/instrument-vxi11.out
echo "SCPI-CLIENT PEERS DONE"
