public class Sample {
    Mono<Widget> fetch(String id) {
        return repo.findById(id)
            .map(x -> {
                Widget w = otherService.fetchSync(id).block();
                return w;
            });
    }
}
