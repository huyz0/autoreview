fn parse(value: u32) -> Option<std::num::NonZeroU32> {
    std::num::NonZeroU32::new(value)
}

fn known_positive() -> std::num::NonZeroU32 {
    unsafe { std::num::NonZeroU32::new_unchecked(1) }
}
