fn drain(mut queue: Vec<u64>) {
    while !queue.is_empty() {
        let _item = queue.remove(0);
    }
}
