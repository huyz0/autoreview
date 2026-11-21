class Sample {
    void report() {
        System.getLogger("startup").log(System.Logger.Level.INFO, "region: {0}", System.getenv("SERVICE_REGION"));
        System.getLogger("startup").log(System.Logger.Level.INFO, "credential configured: {0}", System.getenv("SERVICE_API_KEY") != null);
    }
}
