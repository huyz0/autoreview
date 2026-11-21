import io.github.resilience4j.retry.RetryConfig;
class Sample {
    Object config() {
        return RetryConfig.custom().maxAttempts(Integer.MAX_VALUE).build();
    }
}
