@Configuration
public class Sample {
    @Bean
    public RestTemplate restTemplate() {
        return new RestTemplate();
    }
}
