fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().tls_danger_accept_invalid_certs(false).build()
}

fn verified_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .tls_danger_accept_invalid_certs(false)
        .build()
}
