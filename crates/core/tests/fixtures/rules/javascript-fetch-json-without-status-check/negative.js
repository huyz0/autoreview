async function load(url) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(String(response.status));
    return response.json();
}

function forward(url) {
    return fetch(url);
}
