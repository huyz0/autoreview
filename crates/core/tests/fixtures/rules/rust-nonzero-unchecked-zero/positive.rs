fn invalid() -> core::num::NonZero<i32> {
    unsafe { core::num::NonZero::<i32>::new_unchecked(0x0_i32) }
}
