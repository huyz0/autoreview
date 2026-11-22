async function read(url, raw) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(String(response.status));
    const copy = response.clone();
    const value = await response.json();
    const text = await copy.text();
    return raw ? text : value;
}

async function choose(url, raw) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(String(response.status));
    if (raw) return await response.text();
    const value = await response.json();
    return value;
}
