async fn fetch_all(urls: Vec<String>) -> reqwest::Result<()> {
    let client = reqwest::Client::new();
    for url in urls {
        client.get(url).send().await?;
    }
    Ok(())
}

async fn fetch(url: &str) -> reqwest::Result<()> {
    reqwest::get(url).await?;
    Ok(())
}
