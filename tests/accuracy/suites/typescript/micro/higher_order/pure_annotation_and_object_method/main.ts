function helper(x: number) { return x; }
function run(o: object | ((n: number) => number)) { return o; }
function target() { return 1; }

export const pure = /*@__PURE__*/ run({ v: helper(1) });

export const withBody = /*@__PURE__*/ run((n: number) => {
  return helper(n);
});

run({
  m() {
    return target();
  },
});
