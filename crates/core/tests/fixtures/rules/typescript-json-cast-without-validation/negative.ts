type Customer = { name: string };

async function load(url: string) {
    const response = await fetch(url);
    const candidate: unknown = await response.json();
    return customerSchema.parse(candidate);
}

async function decode(input: Decoder) {
    const value = (await input.json()) as Customer;
    return value;
}
