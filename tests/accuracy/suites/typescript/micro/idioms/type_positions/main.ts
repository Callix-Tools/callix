interface Shape { a: number }
type T = { a: number };
function helper(): number { return 1; }
function id<X extends Shape>(x: X): X { return x; }
function f<X>(x: X): X { return x; }
class Box<X> { item: X; constructor(item: X) { this.item = item; } }
type F = typeof helper;
const k: Array<ReturnType<typeof helper>> = [];
const s: Shape = { a: 1 };
const t = s as T;
const u = s satisfies Shape;
f<Shape>(s);
new Box<Shape>(s);
id(s);
