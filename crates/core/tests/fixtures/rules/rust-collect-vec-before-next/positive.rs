fn first_match(values: &[u32]) -> Option<u32> {
    values.iter().copied().filter(|n| *n > 5).collect::<Vec<_>>().into_iter().next()
}
