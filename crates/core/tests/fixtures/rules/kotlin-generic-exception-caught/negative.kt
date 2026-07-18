class Foo {
    fun a() {
        try {
            risky()
        } catch (e: IOException) {
            log(e)
        }
    }
}
