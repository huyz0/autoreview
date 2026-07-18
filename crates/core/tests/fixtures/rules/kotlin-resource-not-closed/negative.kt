class Foo {
    fun a() {
        FileInputStream("x").use { fis ->
            fis.read()
        }
    }
}
