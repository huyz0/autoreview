@Configuration
class Sample {
    @Bean
    fun restTemplate(): RestTemplate {
        return RestTemplate()
    }
}
