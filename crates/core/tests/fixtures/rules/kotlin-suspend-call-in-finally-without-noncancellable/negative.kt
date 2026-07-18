suspend fun b() {
    try {
        doWork()
    } finally {
        withContext(NonCancellable) {
            cleanup()
        }
    }
}

suspend fun doWork() {}
suspend fun cleanup() {}
