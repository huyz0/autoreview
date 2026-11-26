suspend fun fetchAll(ids: List<String>): List<Widget> {
    return ids.map { id ->
        repo.fetch(id)
    }
}

suspend fun register() {
    fun callback() { runBlocking { work() } }
    val anonymous = fun() { runBlocking { work() } }
}

fun ordinary() {
    suspend fun nested() { work() }
    runBlocking { work() }
}
