async function read(url) {
    const response = await fetch(url);
    const value = await response.json();
    const raw = await response.text();
    return { value, raw };
}
