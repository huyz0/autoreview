async fn worker(duration: std::time::Duration) {
    let mut task = tokio::spawn(async { 42 });
    if tokio::time::timeout(duration, &mut task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
    let _result = tokio::time::timeout(duration, async { 42 }).await;
}
