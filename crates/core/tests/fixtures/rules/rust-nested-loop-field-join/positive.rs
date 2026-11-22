fn report(left: &[Row], right: &[Row]) {
    for outer in left {
        for inner in right {
            if outer.id == inner.id {
                report_pair(outer, inner);
            }
        }
    }
}
struct Row { id: u64 }
fn report_pair(_: &Row, _: &Row) {}
