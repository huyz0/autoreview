public class SampleTest {
    void a() {
        when(service.process(anyString(), "literal")).thenReturn(true);
    }
}
