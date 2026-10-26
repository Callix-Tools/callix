function g() {}
function outer() {
  function inner() { g(); }
  inner();
}
outer();
