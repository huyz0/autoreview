import java.util.concurrent.CompletableFuture;

class Main {
    void start(Runnable work, boolean wait) {
        var task = CompletableFuture.runAsync(work);
        if (wait) task.join();
    }
}
