// The probe runtime: a shadow stack of the project's functions. e() records the
// edge (top of the stack -> callee) and pushes, x() pops back to the entry
// depth. An await or yield leaves the stack (s) and re-enters on resume (p), so
// code that runs while a function is suspended is never attributed to it.
// Calls that pass through code that is not instrumented (Array.prototype.map,
// vitest's expect) are attributed to the nearest project function below them.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export function install(outDir) {
  if (globalThis.__callixRt) return globalThis.__callixRt;
  const SHIFT = 1048576; // 2^20 functions
  const table = []; // id -> [file, name, line]
  const bases = new Map(); // file -> first id
  const stack = [];
  let edges = new Map(); // caller * SHIFT + callee -> count
  let flushes = 0;
  const rt = {
    reg(file, fns) {
      let base = bases.get(file);
      if (base === undefined) {
        base = table.length;
        bases.set(file, base);
        for (const [name, line] of fns) table.push([file, name, line]);
      }
      return base;
    },
    e(id) {
      const depth = stack.length;
      if (depth > 0) {
        const key = stack[depth - 1] * SHIFT + id;
        edges.set(key, (edges.get(key) || 0) + 1);
      }
      stack.push(id);
      return depth;
    },
    x(depth) {
      if (stack.length > depth) stack.length = depth;
    },
    s(depth, value) {
      if (stack.length > depth) stack.length = depth;
      return value;
    },
    p(id) {
      stack.push(id);
      return stack.length - 1;
    },
    flush() {
      if (!edges.size) return;
      mkdirSync(outDir, { recursive: true });
      const out = [];
      for (const [key, count] of edges) {
        out.push(JSON.stringify([table[Math.floor(key / SHIFT)], table[key % SHIFT], count]));
      }
      writeFileSync(join(outDir, `${process.pid}-${flushes++}.jsonl`), out.join("\n") + "\n");
      edges = new Map();
    },
  };
  globalThis.__callixRt = rt;
  return rt;
}
