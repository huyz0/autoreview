public class Foo {
    void a() {
        try {
            risky();
        } catch (IOException e) {
            e.printStackTrace();
        }
    }
}
