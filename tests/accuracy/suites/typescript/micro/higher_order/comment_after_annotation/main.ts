function wrap(o: object) { return o; }
function make() { return 1; }
export const typed: object = /* note */ wrap({ v: make() });
