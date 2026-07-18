public class Foo {
    int a() {
        try {
            return 1;
        } finally {
            cleanup();
        }
    }
}
