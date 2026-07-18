class Foo {
    fun a() {
        try {
            risky()
        } catch (e: IOException) {
            throw IOException("failed")
        }
    }
}
