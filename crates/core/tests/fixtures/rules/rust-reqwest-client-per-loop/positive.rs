async fn fetch_all(urls: Vec<String>) -> reqwest::Result<()> {
    for url in urls {
        reqwest::get(url).await?;
    }
    Ok(())
}
