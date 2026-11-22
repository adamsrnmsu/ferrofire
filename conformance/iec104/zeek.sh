# shellcheck shell=bash disable=SC2154
# No IEC 104 parser in the testpeers image (proto.env); conn.log still records the probe's connections.
want_zeek conn id.resp_p 2404
