public class Sample {
    @Transactional
    public void doWork() {
        repo.save(order);
    }
}
