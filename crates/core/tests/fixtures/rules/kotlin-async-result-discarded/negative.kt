suspend fun doWork(scope: CoroutineScope) {
    val deferred = scope.async { risky() }
    deferred.await()
}
