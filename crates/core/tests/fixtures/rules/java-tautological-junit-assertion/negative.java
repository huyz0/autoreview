import org.junit.jupiter.api.Assertions;
class SampleTest {
    void testResult(int expected, int actual) {
        Assertions.assertEquals(expected, actual);
        Assertions.assertEquals(read(), read());
    }
}
