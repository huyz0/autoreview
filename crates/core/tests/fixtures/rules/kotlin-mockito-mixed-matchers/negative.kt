class SampleTest {
    fun a() {
        `when`(service.process(anyString(), eq("literal"))).thenReturn(true)
    }
}
