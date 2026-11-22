function report(left: {id: number, parentId: number}[], right: {id: number, parentId: number}[]) {
    for (const outer of left) {
        for (const inner of right) {
            if (outer.id === inner.id) console.log(outer, inner);
        }
    }
}
