public class Foo {
    void a() {
        try {
            risky();
        } catch (Exception e) {
            log(e);
        }
    }
}
