class Sample {
    fun fetch(id: String): Mono<Widget> {
        return repo.findById(id)
            .map { x ->
                val w = otherService.fetchSync(id).block()
                w
            }
    }
}
