class Foo {
    fun a() {
        try {
            risky()
        } catch (e: IOException) {
            logger.error("failed", e)
        }
    }
}
