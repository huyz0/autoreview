class Foo {
    fun a(): Int {
        try {
            return 1
        } finally {
            cleanup()
        }
    }
}
