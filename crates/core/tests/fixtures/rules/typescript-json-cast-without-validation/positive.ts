type Customer = { name: string };

async function load(url: string) {
    const response = await fetch(url);
    const customer = (await response.json()) as Customer;
    return customer;
}
