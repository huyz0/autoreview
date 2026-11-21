class Sample {
    void schedule(Runnable work) {
        new Thread(work).start();
        new java.lang.Thread(work).start();
        work.run();
    }
}

class CustomThreads {
    void schedule(Runnable work) {
        new Thread(work).run();
    }

    record Thread(Runnable work) {
        void run() {
            work.run();
        }
    }
}
