class Sample {
    fun configure(http: HttpSecurity) {
        http.csrf().csrfTokenRepository(repo)
    }
}
