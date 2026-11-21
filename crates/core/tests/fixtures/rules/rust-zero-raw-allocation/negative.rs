unsafe fn reserve() -> *mut u8 {
    unsafe { std::alloc::alloc(std::alloc::Layout::from_size_align(8, 8).unwrap()) }
}

fn empty_layout() -> std::alloc::Layout {
    std::alloc::Layout::from_size_align(0, 8).unwrap()
}
