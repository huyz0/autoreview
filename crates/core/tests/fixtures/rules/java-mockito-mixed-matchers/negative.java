public class SampleTest {
    void a() {
        when(service.process(anyString(), eq("literal"))).thenReturn(true);
    }
}
