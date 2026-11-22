use tokio::task::{spawn_blocking as start};

pub async fn parse(data: String) {
    let task = start(move || data.len());
    task.abort();
}
