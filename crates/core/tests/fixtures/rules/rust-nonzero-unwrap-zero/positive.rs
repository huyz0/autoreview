fn invalid() -> std::num::NonZeroU32 {
    std::num::NonZeroU32::new(0).expect("default size")
}
