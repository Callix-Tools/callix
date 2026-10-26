function fallback() {}
function run(cb = fallback) { cb(); }
run();
