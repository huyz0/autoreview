async function run(items: string[]) { items.forEach(item => console.log(item)); await Promise.all(items.map(async item => save(item))); }
