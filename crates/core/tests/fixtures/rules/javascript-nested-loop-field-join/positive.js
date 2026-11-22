function report(left, right) {
    for (const outer of left) {
        for (const inner of right) {
            if (outer.id === inner.id) console.log(outer, inner);
        }
    }
}
