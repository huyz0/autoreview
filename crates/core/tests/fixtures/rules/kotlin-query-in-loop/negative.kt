class Sample {
    fun f(repo: Repo, ids: List<Int>) {
        val users = repo.findById(ids)
    }
}
