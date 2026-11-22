fn matching(values: &[u32]) -> Vec<u32> {
    values.iter().copied().filter(|n| *n > 5).collect()
}

fn first_match(values: &[u32]) -> Option<u32> {
    values.iter().copied().find(|n| *n > 5)
}
