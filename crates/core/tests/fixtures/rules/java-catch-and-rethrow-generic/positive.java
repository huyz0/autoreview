public class S {
    void f() {
        try {
            doThing();
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }
    void doThing() throws Exception {}
}
