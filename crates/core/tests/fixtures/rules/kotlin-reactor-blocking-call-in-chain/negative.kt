class Sample {
    fun fetch(id: String): Mono<Widget> {
        return repo.findById(id)
            .flatMap { x -> otherService.fetchAsync(id) }
    }
}
