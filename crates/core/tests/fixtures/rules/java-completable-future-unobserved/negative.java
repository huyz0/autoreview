import java.util.concurrent.CompletableFuture;

class TaskOwner {
    private CompletableFuture<Void> pending;

    void store(Runnable work) {
        var task = CompletableFuture.runAsync(work);
        pending = task;
    }

    void finish(CompletableFuture<Void> task) {
        task.join();
    }

    void run(Runnable work) {
        var task = CompletableFuture.runAsync(work);
        var alias = task;
        finish(alias);
    }

    CompletableFuture<Void> handOff(Runnable work) {
        return java.util.concurrent.CompletableFuture.runAsync(work);
    }
}
