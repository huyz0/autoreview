class Sample {
    fun a() {
        try {
            risky()
        } catch (e: Exception) {
            log.error("Failed: " + e.message)
        }
    }
}
