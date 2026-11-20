fn element(items: &[u8], index: usize) -> Option<u8> {
    (index < items.len()).then_some(items[index])
}
