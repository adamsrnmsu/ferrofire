# shellcheck shell=bash disable=SC2154
# Zeek with CISA icsnpp-bacnet: the probe's services, the device's answers and errors, discovery and DCC.
want_zeek bacnet pdu_service who_is
want_zeek bacnet pdu_service i_am
want_zeek bacnet pdu_service read_property
want_zeek bacnet pdu_service write_property
want_zeek bacnet result_code unknown-object
want_zeek bacnet_discovery object_type device
want_zeek bacnet_discovery instance_number 1234
want_zeek bacnet_property property present-value
want_zeek bacnet_device_control pdu_service device_communication_control
