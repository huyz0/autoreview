fn element(items: &[u8], index: usize) -> &u8 {
    if index <= items.len() {
        unsafe { items.get_unchecked(index) }
    } else {
        &0
    }
}
