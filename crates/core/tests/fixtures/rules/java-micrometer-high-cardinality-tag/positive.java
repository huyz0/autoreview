import io.micrometer.core.instrument.Counter;
class Stats {
    Object counter(String request) {
        return Counter.builder("requests").tag("requestId", request);
    }
}
