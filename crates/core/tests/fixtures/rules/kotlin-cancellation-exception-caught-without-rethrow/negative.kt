suspend fun doWork() {
    try {
        fetch()
    } catch (e: CancellationException) {
        log.error("failed", e)
        throw e
    }
}
