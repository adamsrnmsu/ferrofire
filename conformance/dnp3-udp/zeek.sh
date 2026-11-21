# shellcheck shell=bash disable=SC2154
# Zeek's DNP3 UDP analyzer + CISA icsnpp-dnp3 logs, as conformance/dnp3/zeek.sh.
want_zeek dnp3 fc_request READ
want_zeek dnp3 fc_reply RESPONSE
want_zeek dnp3_control block_type 'Control Relay Output Block'
want_zeek dnp3_control status_code Success
want_zeek dnp3_control index_number 5
