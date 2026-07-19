public class Sample {
    @Transactional
    private void doWork() {
        repo.save(order);
    }
}
