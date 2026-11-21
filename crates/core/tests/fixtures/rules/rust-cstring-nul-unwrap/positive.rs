fn name() -> std::ffi::CString {
    std::ffi::CString::new("left\u{0}right").expect("valid name")
}
