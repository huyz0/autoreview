fn element(items: &[u8], index: usize) -> Option<u8> {
    (index < items.len()).then(|| items[index])
}

fn check_independent_index(items: &[u8], earlier: &[u8], index: usize) -> Option<u8> {
    let first = (index < earlier.len()).then_some(items[index]);
    first.or((index < items.len()).then_some(items[0]))
}

fn use_a_checked_index(items: &[u8], index: usize) -> Option<u8> {
    items.get(index).copied()
}
