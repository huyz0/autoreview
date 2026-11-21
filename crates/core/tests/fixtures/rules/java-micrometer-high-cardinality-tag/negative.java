import io.micrometer.core.instrument.Counter;
class Stats {
    Object counter(String method) {
        return Counter.builder("requests").tag("method", method);
    }
}
