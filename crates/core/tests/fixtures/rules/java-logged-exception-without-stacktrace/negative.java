public class Sample {
    void a() {
        try {
            risky();
        } catch (Exception e) {
            log.error("Failed", e);
        }
    }
}
