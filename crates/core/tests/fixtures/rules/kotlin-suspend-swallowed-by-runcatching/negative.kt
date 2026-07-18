class Foo {
    fun a(): Int {
        return runCatching {
            1 + 1
        }.getOrDefault(0)
    }
}
