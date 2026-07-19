class SampleTest {
    fun a() {
        `when`(service.process(anyString(), "literal")).thenReturn(true)
    }
}
