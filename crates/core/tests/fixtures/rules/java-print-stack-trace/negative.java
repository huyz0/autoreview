public class Foo {
    void a() {
        try {
            risky();
        } catch (IOException e) {
            logger.error("failed", e);
        }
    }
}
