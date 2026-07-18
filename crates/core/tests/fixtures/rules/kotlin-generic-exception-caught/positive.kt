class Foo {
    fun a() {
        try {
            risky()
        } catch (e: Exception) {
            log(e)
        }
    }
}
