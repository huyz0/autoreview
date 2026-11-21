async fn fetch(url: &str) -> reqwest::Result<String> {
    reqwest::get(url).await?.text().await
}

async fn hand_off(url: String) {
    let _job = tokio::task::spawn_blocking(move || reqwest::blocking::get(url));
}

fn fetch_synchronously(url: &str) -> reqwest::Result<String> {
    reqwest::blocking::get(url)?.text()
}
