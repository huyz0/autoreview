public class Foo {
    void a() {
        try {
            risky();
        } catch (IOException e) {
            log(e);
        }
    }
}
