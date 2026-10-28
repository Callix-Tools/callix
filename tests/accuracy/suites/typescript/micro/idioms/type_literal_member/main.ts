type Api = { run(): void; label: string };
function f(a: Api) {
  if (a.label) a.run();
}
