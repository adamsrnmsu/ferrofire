# shellcheck shell=bash disable=SC2154
# Zeek's modbus analyzer + CISA icsnpp-modbus logs.
want_zeek modbus func READ_COILS
want_zeek modbus func READ_DISCRETE_INPUTS
want_zeek modbus func READ_HOLDING_REGISTERS
want_zeek modbus func READ_INPUT_REGISTERS
want_zeek modbus func WRITE_SINGLE_COIL
want_zeek modbus func WRITE_SINGLE_REGISTER
want_zeek modbus func WRITE_MULTIPLE_COILS
want_zeek modbus func WRITE_MULTIPLE_REGISTERS
want_zeek modbus_detailed address 30000
