public class Foo {
    void a() {
        try {
            risky();
        } catch (IOException e) {
            throw new IOException("failed", e);
        }
    }
}
