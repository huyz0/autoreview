use tokio::task as tasks;

pub async fn calculate(word: String) -> Result<usize, tasks::JoinError> {
    let pending = tasks::spawn_blocking(move || word.len());
    let owned = pending;
    owned.await
}
