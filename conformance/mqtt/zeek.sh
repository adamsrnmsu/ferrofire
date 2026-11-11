# shellcheck shell=bash disable=SC2154
# Zeek's built-in MQTT analyzer: connect / subscribe / publish logs.
want_zeek mqtt_connect client_id ff-plain
want_zeek mqtt_connect client_id ff-spb
want_zeek mqtt_connect will_topic ferrofire/rtu-1/status
want_zeek mqtt_subscribe topics ferrofire/rtu-1/+/set
want_zeek mqtt_publish topic ferrofire/rtu-1/level
want_zeek mqtt_publish topic spBv1.0/waterway/NBIRTH/rtu-1
want_zeek mqtt_publish topic spBv1.0/waterway/NDATA/rtu-1
