public class Foo {
    void a() throws Exception {
        try (FileInputStream fis = new FileInputStream("x")) {
            fis.read();
        }
    }
}
