# shellcheck shell=bash
# bacpypes3 0.1.0 (MIT) for probe_bacnet.py: not in the testpeers image, so pip puts it in a venv (needs the network).
[ -x /opt/v/bin/python ] || { python3 -m venv /opt/v && /opt/v/bin/pip install -q bacpypes3==0.1.0; } >/out/prepare.log 2>&1
