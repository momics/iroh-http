/**
 * Entry point for the Tauri compliance host app.
 *
 * Importing the shared runner kicks off all compliance suites inside the
 * webview (the runner self-executes on import). When it finishes it publishes
 * `window.__irohTestSummary`; we mirror that onto the DOM so a human — and the
 * WebdriverIO spec in `e2e/specs/compliance.e2e.ts` — can observe the outcome.
 */

import "../../runners/tauri.ts";

interface Summary {
  passed: number;
  failed: number;
  total: number;
  failures: string[];
}

const statusEl = document.getElementById("status");
const failuresEl = document.getElementById("failures");

function render(summary: Summary): void {
  if (!statusEl) return;
  const ok = summary.failed === 0 && summary.passed > 0;
  statusEl.dataset.state = ok ? "passed" : "failed";
  statusEl.textContent = `${summary.passed} passed, ${summary.failed} failed out of ${summary.total}`;
  if (failuresEl && summary.failures.length) {
    failuresEl.textContent = summary.failures.join("\n");
  }
}

const poll = setInterval(() => {
  const summary = (globalThis as unknown as { __irohTestSummary?: Summary })
    .__irohTestSummary;
  if (summary) {
    clearInterval(poll);
    render(summary);
  }
}, 100);
