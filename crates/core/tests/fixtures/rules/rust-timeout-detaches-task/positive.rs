async fn worker(duration: std::time::Duration) {
    let _outcome = tokio::time::timeout(duration, tokio::spawn(async { 42 })).await;
}
