async function g() {}
async function f() { await g(); }
f();
