class FooTest {
    @Test
    fun good() = runTest {
        launch {
            doWork()
        }
    }
}
