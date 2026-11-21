class Sample {
    void report() {
        System.getLogger("startup").log(System.Logger.Level.ERROR, "startup failed: {0}", System.getenv("SERVICE_API_KEY"));
    }
}
