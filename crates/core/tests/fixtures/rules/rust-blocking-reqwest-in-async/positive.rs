async fn fetch(url: &str) -> reqwest::Result<String> {
    reqwest::blocking::get(url)?.text()
}
