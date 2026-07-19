public class Sample {
    Mono<Widget> fetch(String id) {
        return repo.findById(id)
            .flatMap(x -> otherService.fetchAsync(id));
    }
}
