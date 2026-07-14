public class S {
    void f() {
        try {
            doThing();
        } catch (IOException e) {
            throw new CustomIOException(e);
        }
    }
    void doThing() throws Exception {}
}
