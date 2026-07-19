suspend fun fetchAll(ids: List<String>, scope: CoroutineScope): List<Widget> {
    return ids.map { id ->
        scope.async { repo.fetch(id) }.await()
    }
}
