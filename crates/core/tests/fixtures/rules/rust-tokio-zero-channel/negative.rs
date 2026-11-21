fn events() {
    let (_send, _receive) = tokio::sync::mpsc::channel::<String>(8);
    let (_send, _receive) = tokio::sync::broadcast::channel::<u32>(1);
    let (_send, _receive) = std::sync::mpsc::sync_channel::<u32>(0);
}
