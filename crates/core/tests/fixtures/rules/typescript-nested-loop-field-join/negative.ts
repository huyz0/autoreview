function report(left: {id: number, parentId: number}[], right: {id: number, parentId: number}[]) {
    for (const outer of left) {
        for (const inner of right) {
            if (outer.id === outer.parentId) console.log(outer, inner);
        }
    }
}

function shadowed(left: {id: number, parentId: number}[], right: {id: number, parentId: number}[]) {
    for (const item of left) {
        for (const item of right) {
            if (item.id === item.parentId) console.log(item);
        }
    }
}
