public class Sample {
    public void configure(HttpSecurity http) throws Exception {
        http.csrf().disable();
    }
}
