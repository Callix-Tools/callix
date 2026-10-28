interface Handler { name: string; handle(): void }
function helper() {}
function label(): string { return "h"; }
const h: Handler = { name: label(), handle() { helper(); } };
function run(x: Handler) { x.name = x.name + "!"; if (x.name.length > 0) x.handle(); }
run(h);
