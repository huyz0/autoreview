suspend fun a() {
    try {
        doWork()
    } finally {
        cleanup()
    }
}

suspend fun doWork() {}
suspend fun cleanup() {}
