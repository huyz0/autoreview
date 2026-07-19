suspend fun doWork(scope: CoroutineScope) {
    scope.async { risky() }
}
