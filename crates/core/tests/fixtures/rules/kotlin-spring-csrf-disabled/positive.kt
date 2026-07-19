class Sample {
    fun configure(http: HttpSecurity) {
        http.csrf().disable()
    }
}
