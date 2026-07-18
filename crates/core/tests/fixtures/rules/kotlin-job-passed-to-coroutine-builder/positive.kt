class Foo {
    fun start(scope: CoroutineScope) {
        scope.launch(Job()) {
            doWork()
        }
    }

    suspend fun doWork() {}
}
