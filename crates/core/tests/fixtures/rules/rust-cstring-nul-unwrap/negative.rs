fn names() -> [std::ffi::CString; 3] {
    [
        std::ffi::CString::new("left\\0right").unwrap(),
        std::ffi::CString::new(r"left\0right").unwrap(),
        std::ffi::CString::new("left\x00right").unwrap_or_default(),
    ]
}
