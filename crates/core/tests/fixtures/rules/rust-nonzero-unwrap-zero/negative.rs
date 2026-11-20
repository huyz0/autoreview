fn zero() -> Option<std::num::NonZeroU32> {
    std::num::NonZeroU32::new(0_u32)
}

fn fallback(value: std::num::NonZeroU32) -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(0).unwrap_or(value)
}

fn known_positive() -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(1).unwrap()
}
