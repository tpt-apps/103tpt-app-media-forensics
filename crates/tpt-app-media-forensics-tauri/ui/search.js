// Search result rendering.
//
// The count is stated as "N of M" whenever the limit hid matches. Showing "200
// results" beside 200 rows when 4,000 matched is a false statement about the
// case, and the model carries `total` precisely so this text can be honest.

import { html } from "./dom.js";

/** Renders a search result. */
export function renderSearch(result) {
  const wrap = document.createDocumentFragment();

  const heading = document.createElement("h2");
  heading.textContent = result.truncated
    ? `Search: ${result.returned} of ${result.total} matches (truncated by limit)`
    : `Search: ${result.returned} match${result.returned === 1 ? "" : "es"}`;
  wrap.append(heading);

  if (!result.hits.length) {
    const empty = document.createElement("p");
    empty.className = "placeholder";
    empty.textContent = `Nothing in this case matched ${result.term || "the empty term"}.`;
    wrap.append(empty);
    return wrap;
  }

  const table = document.createElement("table");
  table.innerHTML =
    "<thead><tr><th>Scope</th><th>Reference</th><th>Severity</th><th>Summary</th></tr></thead>";
  const body = document.createElement("tbody");
  for (const hit of result.hits) {
    const row = document.createElement("tr");
    row.innerHTML =
      html`<td class="dim">${hit.scope}</td>` +
      html`<td><code>${hit.reference}</code></td>` +
      html`<td class="${hit.severity ? `sev sev-${hit.severity}` : "dim"}">${hit.severity ?? "-"}</td>` +
      html`<td>${hit.summary}</td>`;
    body.append(row);
  }
  table.append(body);
  wrap.append(table);
  return wrap;
}
