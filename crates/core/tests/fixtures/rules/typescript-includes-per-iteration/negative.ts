function run(items: string[]) { const allowed = new Set(); for (const item of items) { if (allowed.has(item)) console.log(item); } }
