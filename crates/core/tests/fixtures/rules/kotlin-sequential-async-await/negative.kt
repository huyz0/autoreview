suspend fun fetchAll(ids: List<String>, scope: CoroutineScope): List<Widget> {
    val deferreds = ids.map { id -> scope.async { repo.fetch(id) } }
    return deferreds.map { it.await() }
}
