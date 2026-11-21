unsafe fn reserve() -> *mut u8 {
    unsafe { std::alloc::alloc_zeroed(std::alloc::Layout::from_size_align(0, 8).unwrap()) }
}
