async function load(url) {
    const response = await fetch(url);
    return response.json();
}
