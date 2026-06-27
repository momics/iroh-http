/**
 * E2E spec: drive the Tauri host app (which runs every shared compliance suite
 * inside a real WebKit webview) and assert that none of them failed.
 *
 * The host app publishes `window.__irohTestSummary` once the runner completes;
 * we poll for it and fail the CI job if any compliance test failed.
 */

describe("iroh-http compliance suites (Tauri webview)", () => {
  it("runs all suites and reports zero failures", async () => {
    // The runner creates QUIC nodes and exercises networking, so give it room.
    await browser.waitUntil(
      async () => {
        const state = await $("#status").getAttribute("data-state");
        return state === "passed" || state === "failed";
      },
      {
        timeout: 180000,
        interval: 500,
        timeoutMsg: "compliance runner did not finish within 180s",
      },
    );

    const summary = await browser.execute(
      () => globalThis.__irohTestSummary,
    );

    if (!summary) {
      throw new Error("runner did not publish a result summary");
    }

    console.log(
      `compliance summary: ${summary.passed} passed, ${summary.failed} failed out of ${summary.total}`,
    );
    if (summary.failures.length) {
      console.error("failures:\n  - " + summary.failures.join("\n  - "));
    }

    expect(summary.passed).toBeGreaterThan(0);
    expect(summary.failed).toBe(0);
  });
});
