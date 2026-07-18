import java.io.File;

public class Foo {
    void cleanup(File f) {
        boolean ok = f.delete();
        if (!ok) {
            throw new RuntimeException("delete failed");
        }
    }
}
