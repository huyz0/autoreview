fn drain(mut queue: std::collections::VecDeque<u64>) {
    while !queue.is_empty() {
        let _item = queue.pop_front();
    }
}
