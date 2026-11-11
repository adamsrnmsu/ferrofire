# shellcheck shell=bash disable=SC2154
# Zeek has no SCPI analyzer; its built-in conn.log records every connection (the 2000 of the churn included).
want_zeek conn id.resp_p 5025
