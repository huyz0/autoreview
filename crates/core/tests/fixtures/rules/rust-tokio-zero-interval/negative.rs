async fn tick(start: tokio::time::Instant) {
    let mut interval = tokio::time::interval_at(start, std::time::Duration::from_millis(1));
    interval.tick().await;
    tokio::time::sleep(std::time::Duration::ZERO).await;
}
