class Sample {
    void schedule(Runnable work) {
        new Thread(work).run();
    }
}
