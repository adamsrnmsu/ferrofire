// A minimal Sparkplug B host application for the conformance harness, built on Eclipse Tahu (EPL-2.0) for the payload codec and
// Eclipse Paho Java for MQTT. It is a test peer: ferrofire links none of this.
//
//   java -cp 'lib/*:.' TahuHost <broker-url> <group> <edge-node>
//
// Prints one line per decoded message ("NBIRTH seq=0 bdSeq=0 level=250.5(alias 1) ..."), then drives the edge node: it sends an NCMD
// with Node Control/Rebirth and expects the edge node's NBIRTH, then an NCMD that writes setpoint=55.0 (by name), and lets the
// edge node's NDATA run for 3 s. Exit 1 if the NBIRTH never comes.
import java.util.*;
import java.util.concurrent.*;
import org.eclipse.paho.client.mqttv3.*;
import org.eclipse.tahu.message.SparkplugBPayloadDecoder;
import org.eclipse.tahu.message.SparkplugBPayloadEncoder;
import org.eclipse.tahu.message.model.*;

public class TahuHost {
    static final BlockingQueue<String> events = new LinkedBlockingQueue<>();

    public static void main(String[] a) throws Exception {
        String group = a[1], edge = a[2], ncmd = "spBv1.0/" + group + "/NCMD/" + edge;
        MqttClient c = new MqttClient(a[0], "tahu-host-" + System.nanoTime(), null);
        c.setCallback(new MqttCallback() {
            public void connectionLost(Throwable t) { System.out.println("connection lost: " + t); }
            public void deliveryComplete(IMqttDeliveryToken t) {}
            public void messageArrived(String topic, MqttMessage m) throws Exception {
                String[] p = topic.split("/");
                String type = p[2];
                SparkplugBPayload pl = new SparkplugBPayloadDecoder().buildFromByteArray(m.getPayload(), null);
                StringBuilder sb = new StringBuilder(type + " seq=" + pl.getSeq());
                for (Metric x : pl.getMetrics())
                    sb.append(' ').append(x.getName()).append('=').append(x.getValue()).append("(alias ").append(x.getAlias()).append(", ").append(x.getDataType()).append(')');
                System.out.println(sb);
                events.add(type + (type.equals("NDATA") ? ":" + pl.getMetrics().size() : ""));
            }
        });
        c.connect();
        c.subscribe("spBv1.0/" + group + "/#", 1);
        // what a host does on start-up: it has missed the NBIRTH, so it asks for a new one
        send(c, ncmd, new Metric.MetricBuilder("Node Control/Rebirth", MetricDataType.Boolean, true).createMetric());
        expect("NBIRTH");
        System.out.println("REBIRTH OK");
        send(c, ncmd, new Metric.MetricBuilder("setpoint", MetricDataType.Double, 55.0).createMetric());
        System.out.println("NCMD WRITE SENT");
        Thread.sleep(3000);
        c.disconnect();
        System.out.println("TAHU HOST DONE");
    }

    static void send(MqttClient c, String topic, Metric m) throws Exception {
        SparkplugBPayload p = new SparkplugBPayload.SparkplugBPayloadBuilder().setTimestamp(new Date()).addMetric(m).createPayload();
        c.publish(topic, new SparkplugBPayloadEncoder().getBytes(p, false), 0, false);
    }

    static void expect(String type) throws Exception {
        long end = System.currentTimeMillis() + 20000;
        while (System.currentTimeMillis() < end) {
            String e = events.poll(500, TimeUnit.MILLISECONDS);
            if (e != null && e.startsWith(type)) return;
        }
        System.out.println("TIMEOUT waiting for " + type);
        System.exit(1);
    }
}
