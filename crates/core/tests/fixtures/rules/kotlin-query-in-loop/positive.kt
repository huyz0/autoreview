class Sample {
    fun f(repo: Repo, ids: List<Int>) {
        for (id in ids) {
            val user = repo.findById(id)
        }
    }
}
