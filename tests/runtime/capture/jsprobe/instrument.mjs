// Source transform: wrap every function body of the project in enter/exit
// probes. Positions come from the ORIGINAL source and only text is inserted on
// existing lines, so every recorded line is a line of the checkout.
//
// Names follow ECMAScript function names (`f`, `Class.m`, `Class.get x`,
// `const f = () => ..` is `f`), qualified by the enclosing named scopes
// (functions, classes, bindings). A function without a name is
// `<scope>.<anonymous>` (`<anonymous#2>`, ... in source order), and a member of
// an object literal is `<scope>.<object>.key` (`<object#2>`, ...). The `get`/`set`
// of a property descriptor (`Object.defineProperty(o, "k", {get() {}})`) are
// `get k`/`set k`, like a written accessor. See tests/runtime/README.md.
import { parse } from "@babel/parser";

const FUNCTION_TYPES = new Set([
  "FunctionDeclaration", "FunctionExpression", "ArrowFunctionExpression",
  "ObjectMethod", "ClassMethod", "ClassPrivateMethod",
]);
const WRAPPERS = new Set([
  "ParenthesizedExpression", "TSAsExpression", "TSSatisfiesExpression",
  "TSNonNullExpression", "TSTypeAssertion", "TSInstantiationExpression",
]);

function keyName(key, computed) {
  if (computed) return null;
  if (!key) return null;
  if (key.type === "Identifier") return key.name;
  if (key.type === "StringLiteral") return key.value;
  if (key.type === "NumericLiteral") return String(key.value);
  if (key.type === "PrivateName") return "#" + key.id.name;
  return null;
}

// ECMAScript NamedEvaluation: the name a function gets from where it is written.
function ownName(node, parent) {
  if (node.type === "FunctionDeclaration") return node.id ? node.id.name : "default";
  if (node.type === "FunctionExpression" && node.id) return node.id.name;
  if (node.type === "ObjectMethod" || node.type === "ClassMethod" || node.type === "ClassPrivateMethod") {
    const base = keyName(node.key, node.computed);
    if (base == null) return null;
    if (node.kind === "get") return "get " + base;
    if (node.kind === "set") return "set " + base;
    return base;
  }
  if (!parent) return null;
  switch (parent.type) {
    case "VariableDeclarator":
      return parent.id.type === "Identifier" && unwrap(parent.init) === node ? parent.id.name : null;
    case "AssignmentExpression":
      return parent.left.type === "Identifier" && unwrap(parent.right) === node ? parent.left.name : null;
    // a parameter default (`mapper = (i) => i`) is a value, like an argument: anonymous
    case "ObjectProperty":
      return unwrap(parent.value) === node ? keyName(parent.key, parent.computed) : null;
    case "ClassProperty":
    case "ClassPrivateProperty":
      return unwrap(parent.value) === node ? keyName(parent.key, parent.computed) : null;
    default:
      return null;
  }
}

// A name a non-function node gives to the functions inside it (a class, or a
// binding whose initializer holds them).
function scopeName(node, parent) {
  if (node.type === "ClassDeclaration" || node.type === "ClassExpression") {
    if (node.id) return node.id.name;
    if (parent && parent.type === "VariableDeclarator" && parent.id.type === "Identifier") return parent.id.name;
    return null;
  }
  if (node.type === "VariableDeclarator" && node.id.type === "Identifier") return node.id.name;
  return null;
}

function unwrap(expr) {
  while (expr && WRAPPERS.has(expr.type)) expr = expr.expression;
  return expr;
}


// The property an accessor descriptor defines: `Object.defineProperty(o, "k", {get() {}})`
// and `Object.defineProperties(o, {k: {get() {}}})`. Its `get`/`set` are named
// `get k` / `set k` (`[computed]` for a computed key), like a written accessor.
function isCallTo(node, objects, method) {
  if (!node || node.type !== "CallExpression") return false;
  const c = node.callee;
  return c.type === "MemberExpression" && !c.computed && c.object.type === "Identifier"
    && objects.includes(c.object.name) && c.property.type === "Identifier" && c.property.name === method;
}
function propertyLabel(arg) {
  if (arg && arg.type === "StringLiteral") return arg.value;
  return "[computed]";
}
let descriptorParents = new WeakMap();
function descriptorLabel(objectNode, parent) {
  if (isCallTo(parent, ["Object", "Reflect"], "defineProperty") && unwrap(parent.arguments[2]) === objectNode) {
    return propertyLabel(unwrap(parent.arguments[1]));
  }
  const holder = descriptorParents.get(objectNode);
  return holder === undefined ? null : holder;
}

export function instrument(code, file) {
  descriptorParents = new WeakMap();
  const ast = parse(code, {
    sourceType: "module",
    plugins: ["typescript", "decorators-legacy"],
    createParenthesizedExpressions: true,
    errorRecovery: false,
  });
  const fns = []; // [name, line]
  const inserts = []; // {pos, close, depth, text}
  const add = (pos, close, depth, text) => inserts.push({ pos, close, depth, text, seq: inserts.length });

  const counters = new Map();
  const nth = (key) => { const n = (counters.get(key) || 0) + 1; counters.set(key, n); return n; };
  const numbered = (base, scope) => { const n = nth(base + "\0" + scope.join(".")); return n === 1 ? `<${base}>` : `<${base}#${n}>`; };

  function visitFunction(node, parent, scope, depth, forced) {
    if (!node.body) return; // overload / declare / abstract
    const name = forced != null ? forced : ownName(node, parent);
    const id = fns.length;
    const qual = name == null ? [...scope, numbered("anonymous", scope)] : [...scope, name];
    fns.push([qual.join("."), node.loc.start.line]);
    const probe = `__rt.e(__B+${id})`;
    const isAsync = !!node.async;
    const isGen = !!node.generator;
    const decl = isAsync || isGen ? `let __c=${probe},__t;` : `const __c=${probe};`;
    if (node.body.type === "BlockStatement") {
      if (node.body.start + 1 === node.body.end - 1) {
        add(node.body.start + 1, false, depth, `${decl}try{}finally{__rt.x(__c)}`);
      } else {
        add(node.body.start + 1, false, depth, `${decl}try{`);
        add(node.body.end - 1, true, depth, `}finally{__rt.x(__c)}`);
      }
    } else {
      add(node.body.start, false, depth, `{${decl}try{return (`);
      add(node.body.end, true, depth, `)}finally{__rt.x(__c)}}`);
    }
    const inner = name == null ? scope : [...scope, name];
    walk(node.body, node, inner, depth + 1, { id, isAsync, isGen });
    for (const p of node.params || []) walk(p, node, inner, depth + 1, null);
  }

  function walk(node, parent, scope, depth, fnCtx) {
    if (!node || typeof node.type !== "string") return;
    if (FUNCTION_TYPES.has(node.type)) {
      if (node.type === "ClassMethod" || node.type === "ClassPrivateMethod" || node.type === "ObjectMethod") {
        if (node.computed && node.key) walk(node.key, node, scope, depth, fnCtx);
      }
      const computed = (node.type === "ClassMethod" || node.type === "ClassPrivateMethod") && node.computed;
      visitFunction(node, parent, scope, depth, computed ? "[computed]" : null);
      return;
    }
    if (fnCtx && (node.type === "AwaitExpression" || node.type === "YieldExpression") && node.argument) {
      // suspend: leave the stack; resume: come back on top of whatever runs then
      depth += 1;
      add(node.start, false, depth, `(__t=`);
      // `(__t=await __rt.s(__c, X),__c=__rt.p(__B+id),__t)`
      add(node.argument.start, false, depth, `__rt.s(__c,`);
      add(node.argument.end, true, depth, `),__c=__rt.p(__B+${fnCtx.id}),__t)`);
      walk(node.argument, node, scope, depth + 1, fnCtx);
      return;
    }
    if (fnCtx && node.type === "YieldExpression" && !node.argument) {
      add(node.start, false, depth, `(__t=`);
      add(node.end, true, depth, `(__rt.s(__c,undefined)),__c=__rt.p(__B+${fnCtx.id}),__t)`);
      return;
    }
    if (node.type.startsWith("TS") && node.type !== "TSAsExpression" && node.type !== "TSSatisfiesExpression"
      && node.type !== "TSNonNullExpression" && node.type !== "TSTypeAssertion"
      && node.type !== "TSModuleDeclaration" && node.type !== "TSModuleBlock"
      && node.type !== "TSExportAssignment" && node.type !== "TSParameterProperty"
      && node.type !== "TSInstantiationExpression" && node.type !== "TSEnumDeclaration"
      && node.type !== "TSEnumMember") return; // types hold no code
    if (node.type === "ObjectExpression") {
      // a member of an object literal is written `<scope>.<object>.key`: callix
      // models it as an inline closure of the scope, so it maps by position
      let marker = null;
      const accessorLabel = descriptorLabel(node, parent);
      for (const prop of node.properties) {
        const value = prop.type === "ObjectProperty" ? unwrap(prop.value) : null;
        const fn = prop.type === "ObjectMethod" ? prop : (value && FUNCTION_TYPES.has(value.type) ? value : null);
        if (!fn) { walk(prop, node, scope, depth, fnCtx); continue; }
        if (marker == null) marker = numbered("object", scope);
        if (prop.computed) walk(prop.key, prop, scope, depth, fnCtx);
        let forced = null;
        const key = keyName(prop.key, prop.computed);
        if (accessorLabel != null && prop.type === "ObjectMethod" && prop.kind === "method" && (key === "get" || key === "set")) forced = `${key} ${accessorLabel}`;
        if (accessorLabel != null && prop.type === "ObjectProperty" && (key === "get" || key === "set")) forced = `${key} ${accessorLabel}`;
        if (forced == null && prop.computed) forced = "[computed]";
        visitFunction(fn, prop.type === "ObjectMethod" ? null : prop, [...scope, marker], depth, forced);
      }
      return;
    }
    if (isCallTo(node, ["Object"], "defineProperties") || isCallTo(node, ["Object"], "create")) {
      const map = unwrap(node.arguments[1]);
      if (map && map.type === "ObjectExpression") {
        for (const prop of map.properties) {
          const value = prop.type === "ObjectProperty" ? unwrap(prop.value) : null;
          if (value && value.type === "ObjectExpression") {
            const k = keyName(prop.key, prop.computed);
            descriptorParents.set(value, k == null ? "[computed]" : k);
          }
        }
      }
    }
    if (WRAPPERS.has(node.type)) {
      // `(f)`, `f as T`: the function keeps the name its context gives it
      walk(node.expression, parent, scope, depth, fnCtx);
      return;
    }
    let nextScope = scope;
    const own = scopeName(node, parent);
    if (own != null) {
      // a binding whose value is a function names the function, not a scope
      const init = node.type === "VariableDeclarator" ? unwrap(node.init) : null;
      if (!(init && FUNCTION_TYPES.has(init.type))) nextScope = [...scope, own];
    }
    if (node.type === "TSModuleDeclaration" && node.id && node.id.type === "Identifier") nextScope = [...scope, node.id.name];
    for (const key of Object.keys(node)) {
      if (key === "loc" || key === "start" || key === "end" || key === "extra" || key === "leadingComments"
        || key === "trailingComments" || key === "innerComments" || key === "typeAnnotation"
        || key === "returnType" || key === "typeParameters" || key === "superTypeParameters") continue;
      const value = node[key];
      if (Array.isArray(value)) for (const child of value) walk(child, node, nextScope, depth, fnCtx);
      else if (value && typeof value.type === "string") walk(value, node, nextScope, depth, fnCtx);
    }
  }

  walk(ast.program, null, [], 0, null);
  if (!fns.length) return null;
  // apply: at one position, closes before opens; inner closes first, outer opens first
  inserts.sort((a, b) => a.pos - b.pos
    || (a.close === b.close ? (a.close ? b.depth - a.depth : a.depth - b.depth) : (a.close ? -1 : 1))
    || a.seq - b.seq);
  let out = "";
  let last = 0;
  for (const ins of inserts) {
    out += code.slice(last, ins.pos) + ins.text;
    last = ins.pos;
  }
  out += code.slice(last);
  const header = `const __rt=globalThis.__callixRt;const __B=__rt.reg(${JSON.stringify(file)},${JSON.stringify(fns)});`;
  return { code: header + out, functions: fns.length };
}
