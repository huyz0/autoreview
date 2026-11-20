use std::time::Duration;

async fn run() {
    fn blocking_helper() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let sync_worker = move || std::fs::read("data.txt");
    let _handle = std::thread::spawn(sync_worker);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _ = tokio::fs::read("data.txt").await;
}
