function cb() {}
function run({ handler }: { handler: () => void }) { handler(); }
run({ handler: cb });
