async function load(x: number) { return x; }
async function main2() {
    await Promise.all([1, 2].map(load));
}
