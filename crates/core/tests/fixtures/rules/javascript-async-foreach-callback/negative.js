async function run(items) { items.forEach(item => console.log(item)); await Promise.all(items.map(async item => save(item))); }
