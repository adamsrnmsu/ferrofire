# shellcheck shell=bash disable=SC2154
# Zeek has no SCPI analyzer; its built-in conn.log records the controller's connections to the instruments (5025, and the portmapper on 111 for VXI-11).
want_zeek conn id.resp_p 5025
want_zeek conn id.resp_p 111
