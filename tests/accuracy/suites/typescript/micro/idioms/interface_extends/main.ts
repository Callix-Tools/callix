interface A { m(): void }
interface B extends A {}
function use(b: B) { b.m(); }
