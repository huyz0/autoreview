function run(n: number) {
    let i = 0;
    for (; n >= i; ++i) console.log(i);
    for (let j = n; j > 0; j--) console.log(j);
    let other = 100;
    for (let k = 0; k < n; other--) { k++; }
}
