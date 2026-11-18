# shellcheck shell=bash disable=SC2154
# Port 5502 is not Zeek's Modbus port; its conn.log still records the client's connections.
want_zeek conn id.resp_p 5502
