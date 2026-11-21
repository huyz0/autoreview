async fn send_reports(url: &str, token: &str) -> reqwest::Result<()> {
    reqwest::Client::new()
        .get("https://service.example.invalid/report")
        .bearer_auth(token)
        .send()
        .await?;
    reqwest::Client::new()
        .get("http://service.example.invalid/report")
        .send()
        .await?;
    reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?;
    let _unpolled = reqwest::Client::new()
        .get("http://service.example.invalid/report")
        .bearer_auth(token)
        .send();
    Ok(())
}

fn local_services(token: &str) -> reqwest::Result<()> {
    reqwest::blocking::Client::new()
        .get("http://TEST.Localhost.:8080/report")
        .bearer_auth(token)
        .send()?;
    reqwest::blocking::Client::new()
        .get("http://127.42.0.9:8080/report")
        .bearer_auth(token)
        .send()?;
    reqwest::blocking::Client::new()
        .get("http://[::ffff:127.0.0.1]:8080/report")
        .bearer_auth(token)
        .send()?;
    reqwest::blocking::Client::new()
        .get("http://service.example.invalid@localhost:8080/report")
        .bearer_auth(token)
        .send()?;
    Ok(())
}
