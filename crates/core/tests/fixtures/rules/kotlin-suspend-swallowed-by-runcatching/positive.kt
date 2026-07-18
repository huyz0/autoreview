class Foo {
    suspend fun a(): Int {
        return runCatching {
            fetch()
        }.getOrDefault(0)
    }

    suspend fun fetch(): Int = 1
}
