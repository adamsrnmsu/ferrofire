"""Eclipse Paho (an MQTT client independent of rumqttc): reads the plain topics, writes commands on .../set, and checks them against the
stand-in provider's log. Exit 1 on the first thing that is wrong."""
import sys, threading, time
import paho.mqtt.client as mqtt

PROVIDER_LOG = sys.argv[1]
seen, lock = {}, threading.Lock()

def on_message(c, u, m):
    with lock: seen[m.topic] = (m.payload.decode(), m.retain)

c = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id="paho-peer")
c.on_message = on_message
c.connect("127.0.0.1", 1883)
c.subscribe("ferrofire/rtu-1/#", qos=1)
c.loop_start()

def wait(what, ok, secs=15):
    end = time.time() + secs
    while time.time() < end:
        if ok(): print("PASS paho: " + what); return
        time.sleep(0.2)
    print("FAIL paho: " + what); sys.exit(1)

def provider_has(s):
    try: return s in open(PROVIDER_LOG).read()
    except FileNotFoundError: return False

with lock: pass
wait("status online", lambda: seen.get("ferrofire/rtu-1/status", ("",))[0] == "online")
wait("level 250.5 read", lambda: seen.get("ferrofire/rtu-1/level", ("",))[0] == "250.5")
wait("breaker state read", lambda: seen.get("ferrofire/rtu-1/brkr", ("",))[0] == "true")
c.publish("ferrofire/rtu-1/setpoint/set", "99999", qos=1).wait_for_publish()   # above max-value: refused
c.publish("ferrofire/rtu-1/setpoint/set", "420.5", qos=1).wait_for_publish()
wait("setpoint command reached the provider", lambda: provider_has("load-mw-setpoint:420.500000"))
c.publish("ferrofire/rtu-1/breaker-cmd/set", "on", qos=1).wait_for_publish()
wait("breaker command reached the provider", lambda: provider_has("load-breaker-toggle:true"))
time.sleep(2)
if provider_has("99999"): print("FAIL paho: an out-of-range command reached the provider"); sys.exit(1)
print("PASS paho: out-of-range command refused")
c.publish("ferrofire/rtu-1/nope/set", "1", qos=1).wait_for_publish()          # unknown tag: logged and skipped
c.loop_stop(); c.disconnect()
