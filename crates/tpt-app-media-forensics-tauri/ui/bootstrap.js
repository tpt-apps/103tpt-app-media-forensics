// Loads before the module graph, and imports nothing on purpose.
//
// # Why this file exists
//
// Every other way this application could fail to start produced a blank window
// with no explanation: a bare specifier that cannot resolve, a module that does
// not parse, a `TypeError` during bootstrap. A forensic tool that opens empty
// looks broken; worse, it looks *quietly* broken, and the analyst has no way to
// tell that from a genuinely empty case.
//
// A classic script with no imports is the only thing guaranteed to run: module
// scripts are deferred and resolved as a graph, so anything that breaks the
// graph also breaks anything that imports it. This file therefore depends on
// nothing, is not a module, and is not subject to the `script-src 'self'`
// policy that forbids inline scripts.
//
// It installs two handlers and then gets out of the way. Both failures below
// are reported the same way - as visible text in the document - because a
// forensic tool should never fail silently.

/** Replaces the empty page with a readable explanation. */
function report(what, detail) {
  const box = document.createElement("pre");
  box.className = "fatal";
  box.textContent = `${what}\n\n${detail}`;
  document.body.replaceChildren(box);
}

addEventListener(
  "error",
  (event) => {
    // A module that fails to load or parse fires `error` on the window with no
    // message on the event itself; the message is on the element.
    const message =
      event.message ||
      (event.target && event.target.src) ||
      (event.error && event.error.message) ||
      "unknown error";
    report("The interface failed to load.", message);
  },
  true,
);

addEventListener("unhandledrejection", (event) => {
  const reason = event.reason;
  const message =
    (reason && reason.message) || (typeof reason === "string" ? reason : "");
  report("The interface hit an unexpected error.", message || String(reason));
});