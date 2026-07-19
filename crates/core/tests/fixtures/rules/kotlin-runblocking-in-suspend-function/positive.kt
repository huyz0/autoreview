suspend fun fetchAll(ids: List<String>): List<Widget> {
    return ids.map { id ->
        runBlocking { repo.fetch(id) }
    }
}
