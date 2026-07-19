class Sample {
    @Transactional
    fun doWork() {
        repo.save(order)
    }
}
