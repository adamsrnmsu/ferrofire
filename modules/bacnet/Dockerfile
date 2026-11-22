# The BACnet module image: ff-physics plus bacpypes3 (and pytest for make modbac-test). Build: make modbac-image
FROM ff-physics:22.04
COPY requirements.txt /opt/ferrofire/modules/bacnet/requirements.txt
RUN pip install --no-cache-dir -r /opt/ferrofire/modules/bacnet/requirements.txt pytest==8.3.4
COPY . /opt/ferrofire/modules/bacnet
WORKDIR /opt/ferrofire/modules/bacnet
