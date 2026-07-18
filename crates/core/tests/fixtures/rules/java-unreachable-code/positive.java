public class Foo {
    void g() {
        throw new RuntimeException("boom");
        System.out.println("dead");
    }
}
