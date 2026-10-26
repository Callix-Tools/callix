function f(n: number): number { return n ? f(n - 1) : 0; }
f(3);
