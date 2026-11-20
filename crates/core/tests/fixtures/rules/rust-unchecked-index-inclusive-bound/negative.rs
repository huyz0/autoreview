fn element(items: &[u8], index: usize) -> &u8 {
    if index < items.len() {
        unsafe { items.get_unchecked(index) }
    } else {
        &0
    }
}

fn prefix(items: &[u8], index: usize) -> &[u8] {
    if index <= items.len() {
        unsafe { items.get_unchecked(..index) }
    } else {
        &[]
    }
}

fn element_after_check(items: &[u8], index: usize) -> &u8 {
    if index <= items.len() {
        if index == items.len() {
            return &0;
        }
        unsafe { items.get_unchecked(index) }
    } else {
        &0
    }
}
