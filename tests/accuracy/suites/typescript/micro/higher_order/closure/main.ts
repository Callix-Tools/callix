function g() {}
function make() { return () => g(); }
const f = make();
f();
