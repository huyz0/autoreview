class Foo {
    fun start(scope: CoroutineScope) {
        scope.launch {
            doWork()
        }
    }

    suspend fun doWork() {}
}
