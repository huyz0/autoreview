class Sample {
    @Transactional
    private fun doWork() {
        repo.save(order)
    }
}
