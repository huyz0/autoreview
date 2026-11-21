async fn send_report(token: &str) -> reqwest::Result<reqwest::Response> {
    reqwest::Client::new()
        .get("http://service.example.invalid:8080/report")
        .bearer_auth(token)
        .send()
        .await
}
