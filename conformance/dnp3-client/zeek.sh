# shellcheck shell=bash disable=SC2154
# Port 20000 is not Zeek's Modbus port; its conn.log still records the client's connections.
want_zeek conn id.resp_p 20000
