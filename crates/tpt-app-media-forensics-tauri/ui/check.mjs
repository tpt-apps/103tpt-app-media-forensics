// Static checks for the frontend, run with `node ui/check.mjs`.
//
// # Why this exists
//
// The frontend has no build step and no test runner, which was a deliberate
// choice - it keeps Node out of the build path of a forensic tool. The cost is
// that nothing checks the JavaScript: `cargo build` compiles the Rust, embeds
// `ui/`, and produces a working executable containing a file that may not parse.
//
// That is not hypothetical. A `return` orphaned outside a function by a text
// edit sat in `screens.js` through four builds and 188 passing Rust tests. The
// application would have opened to a blank screen, because the module fails to
// evaluate before any screen renders. Rust tests cannot see it: they never load
// the frontend, and `cargo build` does not parse JavaScript.
//
// So the checks that would have caught it live here, and are cheap enough to
// run on every build.
//
// # The three failure modes are the same shape
//
// Each of the following produces a build that succeeds, tests that pass, and an
// application that quietly does not work:
//
//   1. A module that does not parse - the whole screen layer never loads.
//   2. An `invoke("...")` naming a command that does not exist - the call is
//      valid JavaScript, the IPC boundary rejects it, and the catch in `guarded`
//      turns it into a banner nobody is watching.
//   3. A screen with no renderer, or an element id with no tag - both return a
//      placeholder or `null` and nothing complains.
//
// The third is the one this file was extended to cover after the first two were
// found: the original bug was type 1, and types 2 and 3 were the same mistake
// waiting to happen.

import { copyFileSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const uiFiles = readdirSync(here).filter((f) => f.endsWith(".js") && f !== "check.mjs");
const sources = new Map(
  uiFiles.map((f) => [f, readFileSync(join(here, f), "utf8")]),
);
const rust = readFileSync(join(here, "..", "src", "commands.rs"), "utf8");
const problems = [];

// ---------------------------------------------------------------- 1. syntax

// `import()` compiles the module without running its top level, so a file that
// imports `@tauri-apps/api` - absent outside the app - still parses. That is the
// boundary wanted: syntax is checked, resolution is not.
for (const [file, source] of sources) {
  try {
    await import(`data:text/javascript;base64,${Buffer.from(source, "utf8").toString("base64")}`);
  } catch (error) {
    if (error instanceof SyntaxError) {
      problems.push(`${file} does not parse: ${error.message}`);
    }
  }
}

// ------------------------------------------------- 2. commands actually exist

const defined = new Set(
  [...rust.matchAll(/#\[tauri::command\]\s*\npub fn (\w+)/g)].map((m) => m[1]),
);
const called = new Set();
for (const source of sources.values()) {
  for (const match of source.matchAll(/invoke\("(\w+)"/g)) {
    called.add(match[1]);
  }
}
for (const command of called) {
  if (!defined.has(command)) {
    problems.push(`the frontend invokes "${command}", which no Rust command defines`);
  }
}

// ------------------------------------------------------ 3. every screen renders

// The `Screen` enum is the backend's own list of screens (spec §79). A variant
// with no renderer in the dispatch table is a screen the application opens and
// then quietly shows a placeholder in - which reads as a bug in the application
// rather than as one missing line of frontend.
//
// The comparison is case-insensitive on purpose. Rust's variants are `Case`
// while the dispatch table keys are `CASE`, because that is what `serde` emits
// (`SCREAMING_SNAKE_CASE`) and what the frontend actually receives. Comparing
// them exactly would report twelve missing renderers on a file that has all
// twelve, which is a check that cries wolf and is therefore not run.
const screens = new Map(
  [...rust.matchAll(/pub enum Screen \{[\s\S]*?\n\}/g)]
    .flatMap((m) => [...m[0].matchAll(/^    (\w+),/gm)].map((x) => x[1]))
    .map((name) => [name.toUpperCase(), name]),
);
const rendered = new Set(
  [...(sources.get("screens.js") ?? "").matchAll(/^    (\w+): render\w+,/gm)].map((m) =>
    m[1].toUpperCase(),
  ),
);
for (const [key, name] of screens) {
  if (!rendered.has(key)) {
    problems.push(`screen ${name} has no renderer in screens.js`);
  }
}
for (const key of rendered) {
  if (!screens.has(key)) {
    problems.push(`screens.js has a renderer for ${key}, which is not a Screen variant`);
  }
}

// ------------------------------------------------ 4. elements the script needs

// `getElementById` returns `null` for a tag that is not in the document, and
// every use of it then fails at the first property access. The failure appears
// as a screen that never renders rather than as a missing element.
const html = readFileSync(join(here, "index.html"), "utf8");
const ids = new Set();
for (const source of sources.values()) {
  // The id pattern includes hyphens. It did not once, and that is exactly how
  // `close-case` - read by app.js, never declared in index.html - survived: the
  // regex `\w+` simply stopped matching before the hyphen, so the check
  // reported every element it looked at as present and never looked at this
  // one. The application crashed at startup on
  // `Cannot read properties of null` - found by running the real webview.
for (const match of source.matchAll(/getElementById\("([\w-]+)"\)/g)) {
    ids.add(match[1]);
  }
}
for (const id of ids) {
  if (!new RegExp(`id="${id}"`).test(html)) {
    problems.push(`the frontend reads element #${id}, which index.html does not define`);
  }
}

// ------------------------------------------------------ 5. imports resolve

// A relative import to a file that was renamed or moved is a module that fails
// to load, and the failure names a path rather than the screen that broke.
for (const [file, source] of sources) {
  for (const match of source.matchAll(/from "\.\/(\w+\.js)"/g)) {
    if (!sources.has(match[1])) {
      problems.push(`${file} imports ./${match[1]}, which does not exist`);
    }
  }
}
// A *bare* specifier is worse than a missing file, and is the reason
// `ui/bridge.js` exists. `frontendDist` serves a directory of static files, so
// there is no import map and no bundler: `import { invoke } from
// "@tauri-apps/api/core"` cannot resolve in a browser at all. It throws before
// a single line of the application runs, the window opens empty, and nothing
// anywhere reports an error. That failure was here before the bridge was
// written, and no amount of parsing would have caught it.
for (const [file, source] of sources) {
  // Static and dynamic imports alike. The separator is tabs and spaces only:
  // allowing `\s` lets a match run across a line break and pick up a quoted
  // phrase inside the comment above the code.
  for (const match of source.matchAll(/(?:from|import\()[ \t]*"([^"\n]*)"/g)) {
    // This module quotes the bare specifier it exists to prevent, in a comment.
    // Linting prose would flag the explanation as the mistake, so comment lines
    // are skipped. Only executable lines are linted.
    const before = source.slice(0, match.index);
    const onComment = /\/\/[^\n]*$|^\s*\*/.test(before.slice(before.lastIndexOf("\n") + 1));
    if (onComment) continue;
    const specifier = match[1];
    // A leading dot is a relative path, and always resolves.
    if (!specifier.startsWith(".")) {
      problems.push(
        `${file} imports "${specifier}". This frontend has no bundler and no ` +
          `import map, so only relative paths resolve. Use ui/bridge.js for ` +
          `anything Tauri provides.`,
      );
    }
  }
}

// --------------------------------------------------- 6. every screen renders

// Static parsing cannot see a `ReferenceError`. All the checks above pass on a
// file that calls a helper nobody passed in: the name resolves at parse time
// and throws at click time, on one screen, in one handler. That is not
// hypothetical - `renderReports` called `guarded` without it being in scope, and
// every check above was green.
//
// So the renderers are actually executed, against a stub DOM, with plausible
// view models. The stub is deliberately thin: it records what the code does and
// throws only where the DOM would throw. A renderer that builds real nodes
// passes; one that reaches for something undefined fails here rather than in
// front of an analyst.

// Method names that must return something callable rather than another
// stand-in, so `view.items.map(x => x.id)` does not return a proxy. Defined
// before `STUB` because the stub string interpolates it.
const ARRAYISH = [
  "map", "filter", "forEach", "slice", "concat", "find", "findIndex", "some",
  "every", "reduce", "sort", "reverse", "flat", "flatMap", "indexOf",
  "includes", "join", "at", "keys", "values", "entries", "toFixed", "padStart",
  "toString", "trim", "toUpperCase", "toLowerCase", "charAt", "split",
];

// Returned verbatim where a renderer branches on a value rather than a shape.
const FIXTURES = {
  screens: [{ screen: "CASE", label: "Case", needs_asset: false }],
};

const STUB = `
class El {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.children = [];
    this.dataset = {};
    this.style = {};
    this.listeners = {};
    this._text = "";
  }
  get textContent() { return this._text; }
  set textContent(v) { this._text = String(v); this.children = []; }
  set innerHTML(v) { this._text = String(v); }
  append(...kids) { for (const k of kids) this.children.push(k); }
  appendChild(k) { this.children.push(k); }
  replaceChildren(...kids) { this.children = kids; }
  remove() {}
  setAttribute() {}
  removeAttribute() {}
  addEventListener(name, fn) { (this.listeners[name] ||= []).push(fn); }
  removeEventListener() {}
  querySelector() { return null; }
  querySelectorAll() { return []; }
  getBoundingClientRect() { return { width: 320, height: 240, left: 0, top: 0 }; }
  focus() {}
  getContext(kind) {
    // Only the 2d context is ever requested. The handful of calls the viewer
    // makes are recorded; a missing one is not this check's business.
    return {
      canvas: this,
      createImageData: (w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(w * h * 4) }),
      getImageData: (x, y, w, h) => ({ width: w, height: h, data: new Uint8ClampedArray(w * h * 4) }),
      putImageData() {}, drawImage() {}, fillRect() {}, clearRect() {},
      fillText() {}, measureText: () => ({ width: 0 }), save() {}, restore() {},
      beginPath() {}, moveTo() {}, lineTo() {}, stroke() {}, translate() {}, scale() {},
      createLinearGradient: () => ({ addColorStop() {} }),
      setTransform() {}, rect() {}, arc() {}, closePath() {},
    };
  }
}
const registry = new Map();
for (const id of ${JSON.stringify([...ids])}) {
  const node = new El("div");
  node.id = id;
  registry.set(id, node);
}
globalThis.document = {
  createElement: (tag) => new El(tag),
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t) }),
  createDocumentFragment: () => new El("#fragment"),
  getElementById: (id) => registry.get(id) ?? null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener() {},
  body: new El("body"),
};
globalThis.window = { setTimeout() {}, clearTimeout() {}, addEventListener() {}, devicePixelRatio: 1 };
globalThis.requestAnimationFrame = (fn) => fn(0);
globalThis.ImageData = class { constructor(d, w, h) { this.data = d; this.width = w; this.height = h; } };

// The Tauri bridge, as \`withGlobalTauri\` injects it. \`ui/bridge.js\` reads
// \`globalThis.__TAURI__\`, so this is what the renderers actually talk to.
function standIn() {
  const t = function stub() {};
  return new Proxy(t, {
    get(_x, p) {
      if (p === Symbol.iterator) return function* empty() {};
      if (p === "length") return 0;
      if (p === "then") return undefined;
      if (${JSON.stringify([...ARRAYISH])}.includes(p)) return () => standIn();
      if (p === Symbol.toPrimitive) return () => "";
      return standIn();
    },
  });
}
globalThis.__TAURI__ = {
  core: { invoke: async (cmd) => (${JSON.stringify(FIXTURES)})[cmd] ?? standIn() },
  event: { listen: async () => () => {} },
  dialog: { open: async () => null },
};
`;

// A view model per command, shaped from the Rust structs the commands return.
// Missing fields read as `undefined` in JavaScript rather than throwing, which
// is the same tolerance the real renderer has to survive.
// A permissive stand-in for every view model.
//
// This check is about *wiring*: does a screen reach for something that is not
// in scope, and does pressing a button throw. It is deliberately not about field
// names. An earlier version hand-wrote a fixture per command and it was wrong
// within one pass - `dashboard` returns `counts` and `analysis_status`, not the
// `by_severity` I guessed - and a fixture that drifts from the Rust struct
// produces failures that have nothing to do with the frontend, which is how a
// check gets switched off.
//
// So every property read yields another stand-in: `.length` is 0 so bounds work,
// iteration yields nothing so `for..of` terminates, and every member is callable
// so method calls do not throw. A renderer that reads
// `view.findings[0].title` therefore runs to completion without the harness
// needing to know what a `Finding` is.
//
// What this deliberately does not cover: a renderer reading a field the backend
// never sends. That is a Rust-side contract, and `view::*` plus the command
// cross-check above are where it belongs. This check answers one question -
// does the screen layer hold together - and it answers it without a fixture to
// maintain every time a struct gains a field.
function standIn() {
  const target = function stub() {};
  return new Proxy(target, {
    get(_target, prop) {
      if (prop === Symbol.iterator) return function* empty() {};
      if (prop === "length") return 0;
      // Not a thenable. Returning a function here would make every `await
      // invoke(...)` wait on a promise that never settles.
      if (prop === "then") return undefined;
      if (ARRAYISH.includes(prop)) return () => standIn();
      if (prop === Symbol.toPrimitive) return () => "";
      return standIn();
    },
  });
}

/**
 * Every element in a rendered subtree that carries a click handler.
 *
 * Walks `children` directly rather than using `querySelectorAll`, because the
 * stub has no selector engine and the tree is small.
 */
function buttonsIn(node, found = []) {
  if (!node || typeof node !== "object") return found;
  if (node.listeners?.click?.length) found.push(node);
  for (const child of node.children ?? []) buttonsIn(child, found);
  return found;
}

const harness = join(tmpdir(), `ui-render-${process.pid}`);
mkdirSync(harness, { recursive: true });
try {
  // Bare `@tauri-apps/*` specifiers cannot resolve outside the app, so they are
  // stubbed into a throwaway `node_modules`. The frontend's own modules are
  // copied in unchanged - the point is to execute what ships, not a rewrite.
  const apiDir = join(harness, "node_modules", "@tauri-apps", "api");
  mkdirSync(apiDir, { recursive: true });
  writeFileSync(join(apiDir, "package.json"), JSON.stringify({ name: "@tauri-apps/api", type: "module", exports: { "./core": "./core.js", "./event": "./event.js" } }));
  writeFileSync(
    join(apiDir, "core.js"),
    `export async function invoke(cmd) {\n` +
      `  const fixed = ${JSON.stringify(FIXTURES)}[cmd];\n` +
      `  return fixed === undefined ? standIn() : structuredClone(fixed);\n` +
      `}\n` +
      `function standIn() {\n` +
      `  const t = function stub() {};\n` +
      `  return new Proxy(t, {\n` +
      `    get(_x, p) {\n` +
      `      if (p === Symbol.iterator) return function* empty() {};\n` +
      `      if (p === "length") return 0;\n` +
      `      if (p === "then") return undefined;\n` +
      `      if (${JSON.stringify([...ARRAYISH])}.includes(p)) return () => standIn();\n` +
      `      if (p === Symbol.toPrimitive) return () => "";\n` +
      `      return standIn();\n` +
      `    },\n` +
      `  });\n` +
      `}\n`,
  );
  writeFileSync(join(apiDir, "event.js"), `export async function listen() { return () => {}; }`);

  const dialogDir = join(harness, "node_modules", "@tauri-apps", "plugin-dialog");
  mkdirSync(dialogDir, { recursive: true });
  writeFileSync(join(dialogDir, "package.json"), JSON.stringify({ name: "@tauri-apps/plugin-dialog", type: "module", exports: "./index.js" }));
  writeFileSync(join(dialogDir, "index.js"), `export async function open() { return null; }`);

  for (const file of uiFiles) copyFileSync(join(here, file), join(harness, file));
  writeFileSync(join(harness, "package.json"), JSON.stringify({ type: "module" }));
  writeFileSync(join(harness, "stub-dom.mjs"), STUB);

  // The DOM must exist before any renderer runs: a module that reads
  // `document` at import time would otherwise fail here for a reason that has
  // nothing to do with the check.
  await import(pathToFileURL(join(harness, "stub-dom.mjs")).href);
  const render = (await import(pathToFileURL(join(harness, "screens.js")).href)).renderScreen;
  // Iterate the *keys* - `REPORTS`, `CASE`, ... - because those are what
  // `serde` emits and therefore exactly what `renderScreen` receives. Passing
  // the Rust variant name (`Reports`) matched no renderer, so every screen
  // quietly rendered its "no renderer yet" placeholder and this whole check
  // passed without rendering anything at all. A vacuous test is worse than no
  // test: it reports coverage that does not exist.
  for (const [name] of screens) {
    const view = {
      asset: standIn(),
      case: standIn(),
      screens: FIXTURES.screens,
      current: name,
      running: null,
      lastResult: null,
    };
    try {
      const node = await render(name, view, {
        banner() {},
        setAsset() {},
        guarded: async (fn) => fn(),
        jumpTo() {},
        // The same shape `attempt` returns, so a screen that branches on `ok`
        // is exercised the way it runs rather than against a stand-in that
        // always succeeds.
        attempt: async (fn) => {
          try {
            return { ok: true, value: await fn(), error: null };
          } catch (error) {
            return { ok: false, value: null, error: error?.message ?? String(error) };
          }
        },
        openFolder(accept) {
          accept("/tmp/folder");
        },
      });
      if (node === undefined) {
        problems.push(`screen ${name} rendered nothing at all`);
        continue;
      }
      // Then press every button the screen produced.
      //
      // Rendering alone is not enough. The report-bundle handler called
      // `guarded` with nothing in scope: the renderer returned cleanly, the
      // screen looked perfect, and the ReferenceError appeared only when an
      // analyst pressed the button - which is the only place anyone runs this
      // code. Building the DOM exercises the first half of a screen; clicking
      // it exercises the second half, and the second half is where every one of
      // these bugs has lived.
      for (const button of buttonsIn(node)) {
        for (const handler of button.listeners.click ?? []) {
          try {
            await handler({ preventDefault() {} });
          } catch (error) {
            problems.push(`screen ${name}: pressing a button threw: ${error.message}`);
          }
        }
      }
    } catch (error) {
      problems.push(`screen ${name} threw while rendering: ${error.message}`);
    }
  }
} finally {
  rmSync(harness, { recursive: true, force: true });
}

// ------------------------------------------------- 6. no untagged interpolation
//
// `html` escapes every interpolation by construction, so the safe form is the
// one that is easiest to type. This makes the unsafe form impossible to commit:
// a bare template literal reaching `innerHTML` is a build failure.
//
// A plain string is allowed - `table.innerHTML = "<thead>..."` has no
// interpolation and no untrusted data in it. The rule is about templates
// specifically, because that is where the `${}` is.
for (const [file, source] of sources) {
  for (const match of source.matchAll(/\.innerHTML =(\s*)`/g)) {
    const line = source.slice(0, match.index).split("\n").length;
    problems.push(
      `${file}:${line} builds innerHTML from a bare template literal. ` +
        `Use the \`html\` tag: .innerHTML = html\`...\` - it escapes every ` +
        `interpolation, and this file renders untrusted container metadata.`,
    );
  }
  // A multi-line assignment joins its segments with a trailing `+`, so the
  // *second* and later templates are not adjacent to `.innerHTML =` and the
  // check above cannot see them. This walks the assignment the way the source
  // is written and flags any segment that is not tagged - a gap that once left
  // three cells of the assets table unescaped while the first was escaped.
  const lines = source.split("\n");
  let inAssignment = false;
  let previousEndedWithPlus = false;
  lines.forEach((text, index) => {
    if (/\.innerHTML\s*=/.test(text)) inAssignment = true;
    const trimmed = text.trimStart();
    if (inAssignment && previousEndedWithPlus && trimmed.startsWith("`")) {
      problems.push(
        `${file}:${index + 1} is an untagged continuation of an innerHTML ` +
          `assignment. Prefix it with \`html\` so its interpolations are escaped.`,
      );
    }
    previousEndedWithPlus = /\+\s*$/.test(text);
    if (inAssignment && /;\s*$/.test(text)) inAssignment = false;
  });
}

// ------------------------------------------------------------- 7. escaping works
//
// The lint above can only check that the tag is *present*. It cannot check that
// the tag does its job, so this exercises `esc` and `html` against the payloads
// a crafted file would actually carry.
{
  // `dom.js` is loaded the way the other modules are: as a data URL. The `ui/`
  // directory has no package.json, so Node would otherwise read a `.js` file
  // there as CommonJS and reject the `export` keyword.
  const domSource = readFileSync(join(here, "dom.js"), "utf8");
  const { esc, html: tag } = await import(
    `data:text/javascript;base64,${Buffer.from(domSource, "utf8").toString("base64")}`
  );
  const XSS = `<img src=x onerror="alert(1)">`;
  const cases = [
    [XSS, "&lt;img src=x onerror=&quot;alert(1)&quot;&gt;"],
    ["a & b", "a &amp; b"],
    ["</script>", "&lt;/script&gt;"],
    ["it's", "it&#39;s"],
    ['say "hi"', "say &quot;hi&quot;"],
  ];
  for (const [input, expected] of cases) {
    const got = esc(input);
    if (got !== expected) {
      problems.push(`esc(${JSON.stringify(input)}) produced ${JSON.stringify(got)}, expected ${JSON.stringify(expected)}`);
    }
  }
  // `&` must be replaced first or the entities introduced below get re-escaped.
  if (esc("&lt;") !== "&amp;lt;") {
    problems.push("esc double-escapes its own output; & must be replaced before <");
  }
  // A missing optional field is absent, not the word "null".
  if (esc(null) !== "" || esc(undefined) !== "") {
    problems.push("esc must render a missing value as empty, not as 'null'");
  }
  // The tag must escape the value and leave the author's markup alone.
  const built = tag`<td class="num">${XSS}</td>`;
  if (built.includes("<img") || !built.startsWith('<td class="num">')) {
    problems.push(`html did not escape an interpolation: ${built}`);
  }
}

if (problems.length > 0) {
  console.error("frontend check failed:");
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}

console.log(
  `frontend check passed: ${uiFiles.length} modules parse, ` +
    `${called.size} commands invoked and defined, ${screens.size} screens checked, ` +
    `${ids.size} elements present, imports resolve, every screen renders.`,
);
