import io.github.resilience4j.retry.RetryConfig;
class Sample {
    Object config() {
        return RetryConfig.custom().maxAttempts(3).build();
    }
    void process(Options options) {
        options.maxAttempts(Integer.MAX_VALUE);
    }
}
