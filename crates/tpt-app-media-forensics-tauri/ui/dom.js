// Building markup from untrusted values, safely.
//
// # Why this module exists
//
// A forensic tool renders whatever the file under examination says about
// itself. Asset names, container brands, codec strings, atom keys and values,
// and finding summaries are all attacker-controlled in exactly the way that
// matters: they arrive inside a file that was chosen *because* it is hostile.
//
// Every one of those was interpolated straight into an `innerHTML` template.
// A container whose brand is `<img src=x onerror=...>` executed in the
// analyst's session, with the case open - which is the worst place for it to
// run, and a genuinely plausible thing for a crafted file to contain.
//
// # The rule
//
// Markup is built with the `html` tag, never with a bare template literal.
// `html` escapes every interpolated value by construction, so a new row cannot
// forget to. The structural markup stays as written:
//
//     row.innerHTML = html`<td class="num">${index}</td><td>${name}</td>`;
//                      ^^^^^ ^^^^^^^^ escapes      ^^^^^^^^^^ escaped
//
// `ui/check.mjs` fails the build if a template literal reaches `innerHTML`
// without the tag, so the safe form is the only one that can be committed.
//
// Escaping the *value* is what this does. It is not a sanitiser: it makes no
// claim that rendered output is safe for any other purpose, and it does not
// decide what a filename is allowed to contain. Those are the model's job.

/**
 * Escapes a value for interpolation into HTML text or a quoted attribute.
 *
 * `&` first: escaping it later would double-escape the ampersands introduced
 * by the other replacements, turning `&amp;` into `&amp;amp;` and corrupting
 * any genuinely-escaped text that arrived from the file.
 *
 * `null` and `undefined` become the empty string rather than the words "null"
 * or "undefined", which is what a missing optional field means to a reader.
 */
export function esc(value) {
  if (value === null || value === undefined) return "";
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/**
 * Tagged template that escapes every interpolation.
 *
 * The literal chunks pass through untouched - the markup is the code's own, and
 * is trusted by construction - and only the values are escaped.
 *
 * Values may be passed pre-escaped by nesting `html` inside `html`, which is
 * what makes a composed cell work: the inner tag escapes once, and the outer
 * tag would otherwise escape the entities it just produced.
 */
export function html(strings, ...values) {
  let out = strings[0];
  for (let i = 0; i < values.length; i += 1) {
    const value = values[i];
    // A nested `html` result is already escaped; re-escaping would show the
    // reader `&lt;b&gt;` instead of the bold the inner template produced.
    out += (value && value[ESCAPED] ? value.value : esc(value)) + strings[i + 1];
  }
  return out;
}

const ESCAPED = Symbol.for("tpt.html.escaped");

/** Marks a string as already escaped. Used by `html` when composing. */
export function raw(value) {
  return { [ESCAPED]: true, value: String(value) };
}