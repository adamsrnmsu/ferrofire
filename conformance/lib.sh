# shellcheck shell=bash disable=SC2154
# Helpers for conformance/<proto>/check.sh and zeek.sh (sourced inside the testpeers container).
FAILS=0
# proto.env may name extra tshark options (TSHARK_EXTRA), e.g. -d tcp.port==10102,tpkt for peers that cannot listen on 102
tshark() { command tshark ${TSHARK_EXTRA:-} "$@"; }
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS+1)); }
pass() { echo "ok:   $*"; }
# tshark field dump of a display filter, one line per packet
fields() { local pcap=$1 filter=$2; shift 2; local args=(); for f in "$@"; do args+=(-e "$f"); done
  tshark -r "$pcap" -Y "$filter" -T fields "${args[@]}" -E separator=/s -E occurrence=a 2>/dev/null; }
# have PCAP FILTER FIELD... : at least one packet; the field values are printed
count() { tshark -r "$1" -Y "$2" 2>/dev/null | wc -l | tr -d ' '; }
# expert_clean PCAP: the capture must be non-empty with 0 malformed frames and no expert Error/Warn rows (tshark -z expert,warn).
# A bad checksum is only a Warn in Wireshark, so Warns count; EXPERT_ALLOW (regex, default below) lists legitimate ones, e.g. a DNP3
# response that sets an IIN bit on purpose. The offending rows are printed.
expert_clean() {
  local pcap=$1 n m e
  n=$(count "$pcap" "$PROTO_FILTER"); m=$(count "$pcap" '_ws.malformed')
  e=$(tshark -r "$pcap" -q -z expert,warn 2>/dev/null | grep -E '^ +[0-9]+ ' | grep -v -E "${EXPERT_ALLOW:-IIN Abnormality}" || true)
  [ "$n" -gt 0 ] && pass "$n $PROTO_FILTER packets decoded" || fail "no $PROTO_FILTER packets in the capture"
  [ "$m" = 0 ] && pass "0 malformed packets" || { fail "$m malformed packets"; tshark -r "$pcap" -Y '_ws.malformed' 2>&1 | head -5 >&2; }
  [ -z "$e" ] && pass "0 expert errors or warnings" || { fail "expert errors/warnings:"; echo "$e" >&2; }
}
# want_field PCAP FILTER FIELD VALUE: some packet matching FILTER has FIELD containing VALUE (word match)
want_field() {
  local pcap=$1 filter=$2 field=$3 value=$4
  if fields "$pcap" "$filter" "$field" | tr ' ,' '\n\n' | grep -q -x -F -- "$value"; then pass "$field=$value ($filter)"
  else fail "$field=$value not seen in packets matching: $filter"; fi
}
# zeek_run PCAP OUTDIR: run Zeek offline with ZEEK_LOAD (the ICSNPP package) ; logs land in OUTDIR/zeek/. ZEEK_OK=0 if it could not run.
zeek_run() {
  local pcap=$1 out=$2/zeek
  mkdir -p "$out"; ZEEK_OK=1
  if ! (cd "$out" && zeek -C -r "$pcap" ${ZEEK_LOAD:-} >zeek.stdout 2>zeek.stderr); then
    ZEEK_OK=0
    if [ "${ZEEK_REQUIRED:-0}" = 1 ]; then fail "zeek -r failed (see zeek/zeek.stderr)"; else echo "SKIP: zeek -r failed, package ${ZEEK_LOAD:-} not installed? (zeek/zeek.stderr)"; fi
  fi
}
# want_zeek LOG COLUMN VALUE [-b]: the log has a row where COLUMN equals VALUE
want_zeek() {
  local log=$out/zeek/$1.log col=$2 val=$3
  [ "${ZEEK_OK:-0}" = 1 ] || return 0
  [ -f "$log" ] || { if [ "${ZEEK_REQUIRED:-0}" = 1 ]; then fail "zeek $1.log missing"; else echo "SKIP: zeek $1.log missing (parser not installed or no traffic)"; fi; return; }
  if zeek-cut "$col" < "$log" | grep -q -x -F -- "$val"; then pass "zeek $1.log $col=$val"; else fail "zeek $1.log has no row with $col=$val"; fi
}
finish() { if [ "$FAILS" = 0 ]; then echo "CONFORMANCE PASS: $proto"; else echo "CONFORMANCE FAIL: $proto ($FAILS failures)" >&2; exit 1; fi; }
# want_transcript GOLDEN TRANSCRIPT [EXPECTED_DIFF]: TRANSCRIPT equals GOLDEN, or, given EXPECTED_DIFF, `diff GOLDEN TRANSCRIPT` is that
# file exactly (the listed divergences: one that goes away fails too, until the list is updated). The diff is left in $out.
want_transcript() {
  local g=$1 t=$2 e=${3:-} d=$out/transcript.diff
  diff "$g" "$t" > "$d"
  if [ -z "$e" ]; then
    [ -s "$d" ] && { fail "transcript differs from $(basename "$g"):"; head -40 "$d" >&2; } || pass "transcript equal to $(basename "$g")"
  elif cmp -s "$e" "$d"; then pass "transcript equal to $(basename "$g") but for the divergences in $(basename "$e")"
  else fail "transcript vs $(basename "$g") is not the list in $(basename "$e"):"; diff "$e" "$d" | head -40 >&2; fi
}
# want_104_sequence PCAP: the transcripts mask N(S) and N(R), so check them here. On each connection every side's I-frames count N(S)
# up from 0, and each N(R) (I- or S-frame) acknowledges no more I-frames than the other side had sent and no fewer than its last one.
# No I-frame decoded at all (a missing pcap, a field tshark lacks: its message shows) is a failure, not a pass.
want_104_sequence() {
  local bad
  bad=$(tshark -r "$1" -Y 'iec60870_104 && !tcp.analysis.retransmission' -T fields -E separator=/t -E occurrence=a -e tcp.stream \
    -e tcp.srcport -e iec60870_104.type -e iec60870_104.tx -e iec60870_104.rx | awk -F'\t' '
    { n = split($3, t, ","); split($4, tx, ","); split($5, rx, ","); i = 0; r = 0; me = $1 " " $2
      for (k = 1; k <= n; k++) {
        typ = substr(t[k], length(t[k])); if (typ == 3) continue; r++
        if (typ == 0) { i++; seen++; if (tx[i] != ns[me]++) print "frame " NR ": N(S) " tx[i] " on " me; sent[$1]++ }
        if (rx[r] > sent[$1] - ns[me] || rx[r] < nr[me]) print "frame " NR ": N(R) " rx[r] " on " me; nr[me] = rx[r] } }
    END { if (!seen) print "no I-frames decoded" }' | head -5)
  [ -z "$bad" ] && pass "N(S) and N(R) in sequence on every connection" || { fail "IEC 104 sequence numbers:"; echo "$bad" >&2; }
}
