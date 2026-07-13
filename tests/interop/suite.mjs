/**
 * Structured, reusable interop test-suite.
 *
 * Groups a set of interop checks into `{ id, group, name, run() }` tests so
 * the same corpus can drive:
 *   - the on-device Tauri Test tab (against a *remote* peer over the LAN), and
 *   - future headless Node / Deno runners (see `run-node.mjs`).
 *
 * Each test's `run` returns:
 *   `{ ok, detail, latencyMs, transport?, skip? }`
 * where `transport` is `"direct" | "relay" | "unknown"` when meaningful.
 *
 * Groups:
 *   - `discovery`      — advertise→browse round-trip, `address` TXT present &
 *                        dialable, rebind re-emit (folds the #334 DNS-SD checks).
 *   - `direct-dial`    — fetch reaches the peer over its direct `ip:port` and the
 *                        transport is DIRECT (not relay).
 *   - `relay-fallback` — fetch still succeeds with only a relay available.
 *   - `http-compliance`— the `cases.json` corpus (`c.id && !c.skip`) plus the
 *                        self-loopback baseline (ADR-015).
 *   - `serve-stop`     — serve → reachable → stop → refused lifecycle sanity.
 *
 * Pure WHATWG — no Node / Deno / framework imports here. Everything platform-
 * specific arrives through the injected `ctx`.
 */

// @ts-nocheck

/** Stable, greppable per-case token (mirrors `IROH_DNSSD_CHECK`). */
export const IROH_INTEROP_CASE = "IROH_INTEROP_CASE";

const now = () =>
  typeof performance !== "undefined" && performance.now
    ? performance.now()
    : Date.now();

const round = (ms) => Math.round(ms * 10) / 10;

/**
 * Determine the transport used to reach a peer from `peerStats`.
 * @returns {"direct" | "relay" | "unknown"}
 */
export async function detectTransport(node, peerId) {
  try {
    const stats = await node.peerStats(peerId);
    if (!stats) return "unknown";
    const paths = Array.isArray(stats.paths) ? stats.paths : [];
    const active = paths.find((p) => p && p.active) ?? paths[0];
    if (active && typeof active.relay === "boolean") {
      return active.relay ? "relay" : "direct";
    }
    if (typeof stats.relay === "boolean") {
      return stats.relay ? "relay" : "direct";
    }
  } catch {
    /* stats unavailable */
  }
  return "unknown";
}

/** ip:port direct addrs only (drop relay URLs). */
function directAddrsOf(peer, isDialableSocketAddr) {
  if (!peer || !Array.isArray(peer.addrs)) return [];
  return peer.addrs.filter((a) =>
    isDialableSocketAddr ? isDialableSocketAddr(a) : /:\d+$/.test(a)
  );
}

/**
 * Build the grouped suite for a given runtime context.
 *
 * @param {object} ctx
 * @param {object}  ctx.node        IrohNode-like (fetch, advertise, browse, serve, peerStats, discoveryInfo).
 * @param {object}  ctx.self        `{ nodeId, platform }`.
 * @param {object|null} ctx.peer    Selected target `{ nodeId, platform, addrs }` or null.
 * @param {(url,init)=>Promise<Response>} ctx.fetch
 * @param {Array}   ctx.cases       Compliance corpus (filtered to `c.id && !c.skip` here defensively).
 * @param {object}  ctx.selfLoopbackCase  ADR-015 baseline case.
 * @param {Function} ctx.runCases   `harness.mjs#runCases`.
 * @param {object}  ctx.handler     Compliance request handler (for serve-stop).
 * @param {object}  ctx.helpers     `{ TXT_KEY_ADDRESS, TXT_KEY_RELAY, isDialableSocketAddr }`.
 * @param {boolean} ctx.isServing   Whether a local service is already bound (testing mode).
 * @returns {Array<{ id: string, name: string, tests: Array }>}
 */
export function buildSuite(ctx) {
  const {
    node,
    self,
    peer,
    cases,
    selfLoopbackCase,
    runCases,
    helpers = {},
    isServing,
  } = ctx;
  // Whether the host runtime actually has a working mDNS/DNS-SD stack. On mobile
  // and desktop Tauri this is true, so a discovery round-trip that observes
  // nothing is a real regression (fail), not an environmental skip. Headless
  // Node/Deno runners without mDNS pass `false` so those cases skip cleanly.
  const mdnsCapable = ctx.mdnsCapable === true;
  const fetch = ctx.fetch ?? ((url, init) => node.fetch(url, init));
  const {
    TXT_KEY_ADDRESS = "address",
    TXT_KEY_RELAY = "relay",
    isDialableSocketAddr,
  } = helpers;

  const peerDirect = directAddrsOf(peer, isDialableSocketAddr);

  // ── discovery ──────────────────────────────────────────────────────────────
  const discoveryTests = [
    {
      id: "discovery-self-dialable",
      group: "discovery",
      name: "self exposes a dialable address or relay",
      async run() {
        const t = now();
        const di = await node.discoveryInfo().catch((e) => {
          throw new Error(`discoveryInfo failed: ${e}`);
        });
        const latencyMs = round(now() - t);
        const dialable = !!di.directAddress &&
          (!isDialableSocketAddr || isDialableSocketAddr(di.directAddress));
        const hasRelay = !!di.relayUrl;
        return {
          ok: dialable || hasRelay,
          latencyMs,
          detail:
            `directAddress=${di.directAddress ?? "null"} relayUrl=${
              di.relayUrl ?? "null"
            }` +
            (dialable ? "" : " (no routable direct addr — relay only)"),
        };
      },
    },
    {
      id: "discovery-advertise-browse",
      group: "discovery",
      name: "advertise → browse round-trip sees self with dialable address TXT",
      async run() {
        const t = now();
        const di = await node.discoveryInfo().catch(() => null);
        // F9: iOS only permits Bonjour types declared in Info.ios.plist, so the
        // suite must use the statically-declared `iroh-http-test` type — a random
        // type is silently denied on iOS and the case degrades to a skip that
        // hides real regressions. Isolation between concurrent advertisers comes
        // from a unique *instance* name (allowed to be dynamic), not the type.
        const service = "iroh-http-test";
        // Keep the instance name within the 63-byte DNS-SD label limit: a short
        // node-id prefix keeps it associable, the random suffix keeps it unique.
        const instance = `${self.nodeId.slice(0, 12)}-s${
          Math.random().toString(36).slice(2, 8)
        }`;
        const ac = new AbortController();
        const txt = { pk: self.nodeId };
        if (di?.directAddress) txt[TXT_KEY_ADDRESS] = di.directAddress;
        if (di?.relayUrl) txt[TXT_KEY_RELAY] = di.relayUrl;

        node
          .advertise({
            serviceName: service,
            instanceName: instance,
            port: 1,
            protocol: "udp",
            txt,
            signal: ac.signal,
          })
          .catch(() => {});

        let seenSelf = false;
        let sawActive = false;
        let addressTxt = null;
        const timer = setTimeout(() => ac.abort(), 5000);
        try {
          for await (
            const rec of node.browse({
              serviceName: service,
              protocol: "udp",
              signal: ac.signal,
            })
          ) {
            if (rec.instanceName !== instance) continue;
            if (rec.isActive) sawActive = true;
            addressTxt = rec.txt?.[TXT_KEY_ADDRESS] ?? addressTxt;
            seenSelf = true;
            break;
          }
        } catch {
          /* aborted / timed out */
        } finally {
          clearTimeout(timer);
          ac.abort();
        }

        const latencyMs = round(now() - t);
        if (!seenSelf) {
          // On an mDNS-capable host (mobile/desktop) not observing our own
          // advertisement is a real failure — e.g. missing plist declaration or
          // a discovery regression. Only headless runtimes without mDNS skip.
          return {
            ok: false,
            skip: !mdnsCapable,
            latencyMs,
            detail: mdnsCapable
              ? "self advertisement NOT observed within 5s on an mDNS-capable host — discovery regression or missing Bonjour declaration"
              : "self advertisement not observed within 5s — mDNS unavailable on this host/runtime",
          };
        }
        const dialable = !!addressTxt &&
          (!isDialableSocketAddr || isDialableSocketAddr(addressTxt));
        return {
          ok: sawActive && (dialable || !di?.directAddress),
          latencyMs,
          detail:
            `isActive=${sawActive} address TXT=${addressTxt ?? "absent"}` +
            (di?.directAddress
              ? dialable
                ? " (dialable ✓)"
                : " (present but NOT dialable ✗ — W1 regression)"
              : " (no direct addr to advertise — relay-only host)"),
        };
      },
    },
    {
      id: "discovery-rebind-reemit",
      group: "discovery",
      name: "re-advertise re-emits the record (rebind, W2)",
      async run() {
        const t = now();
        // F9: use the declared `iroh-http-test` type; isolate this run via a
        // unique instance name so concurrent advertisers don't cross-count.
        const service = "iroh-http-test";
        const instance = `${self.nodeId.slice(0, 12)}-r${
          Math.random().toString(36).slice(2, 8)
        }`;
        let emits = 0;
        const browseAc = new AbortController();

        const browsing = (async () => {
          try {
            for await (
              const rec of node.browse({
                serviceName: service,
                protocol: "udp",
                signal: browseAc.signal,
              })
            ) {
              if (rec.instanceName === instance && rec.isActive) emits += 1;
            }
          } catch {
            /* aborted */
          }
        })();

        const advertiseOnce = async (rev) => {
          const ac = new AbortController();
          node
            .advertise({
              serviceName: service,
              instanceName: instance,
              port: 1,
              protocol: "udp",
              txt: { pk: self.nodeId, rev: String(rev) },
              signal: ac.signal,
            })
            .catch(() => {});
          await new Promise((r) => setTimeout(r, 1500));
          ac.abort();
          await new Promise((r) => setTimeout(r, 400));
        };

        await advertiseOnce(0);
        await advertiseOnce(1); // rebind: a fresh advertisement must re-emit
        await new Promise((r) => setTimeout(r, 600));
        browseAc.abort();
        await browsing;

        const latencyMs = round(now() - t);
        if (emits === 0) {
          return {
            ok: false,
            skip: !mdnsCapable,
            latencyMs,
            detail: mdnsCapable
              ? "no emits observed on an mDNS-capable host — rebind re-emit regression"
              : "no emits observed — mDNS unavailable on this host/runtime",
          };
        }
        return {
          ok: emits >= 2,
          latencyMs,
          detail:
            `observed ${emits} emit(s) across two advertisement generations` +
            (emits >= 2
              ? " (re-emit ✓)"
              : " (expected ≥2 — possible W2 regression)"),
        };
      },
    },
  ];

  // ── direct-dial ────────────────────────────────────────────────────────────
  const directDialTests = [
    {
      id: "direct-dial-fetch",
      group: "direct-dial",
      name: "fetch reaches peer over direct ip:port (transport = DIRECT)",
      async run() {
        if (!peer) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail: "no peer selected",
          };
        }
        if (!peerDirect.length) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail:
              "peer advertised no dialable ip:port (relay-only) — see relay-fallback",
          };
        }
        const t = now();
        let status = null;
        try {
          const res = await fetch(`httpi://${peer.nodeId}/hello`, {
            method: "GET",
            directAddrs: peerDirect,
          });
          status = res.status;
          await res.arrayBuffer();
        } catch (e) {
          return {
            ok: false,
            latencyMs: round(now() - t),
            transport: "unknown",
            detail: `fetch failed: ${e}`,
          };
        }
        const latencyMs = round(now() - t);
        const transport = await detectTransport(node, peer.nodeId);
        const fetchOk = status != null && status < 500;
        // F19: only a measured DIRECT transport proves direct dial. An "unknown"
        // transport (peerStats unavailable) must NOT be counted as a direct pass —
        // it is inconclusive, so skip rather than falsely green.
        if (fetchOk && transport === "unknown") {
          return {
            ok: false,
            skip: true,
            latencyMs,
            transport,
            detail: `HTTP ${status} via ${
              peerDirect[0]
            } — transport undetermined (peerStats unavailable); cannot confirm direct`,
          };
        }
        const ok = fetchOk && transport === "direct";
        return {
          ok,
          latencyMs,
          transport,
          detail:
            `HTTP ${status} via ${peerDirect[0]} — transport=${transport}` +
            (transport === "relay"
              ? " (reached over RELAY, not direct ✗)"
              : ""),
        };
      },
    },
  ];

  // ── relay-fallback ─────────────────────────────────────────────────────────
  const relayTests = [
    {
      id: "relay-fallback-fetch",
      group: "relay-fallback",
      name: "fetch succeeds with only a relay available",
      async run() {
        if (!peer) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail: "no peer selected",
          };
        }
        const t = now();
        let status = null;
        try {
          // Omit directAddrs so the stack must fall back to relay routing.
          const res = await fetch(`httpi://${peer.nodeId}/hello`, {
            method: "GET",
          });
          status = res.status;
          await res.arrayBuffer();
        } catch (e) {
          return {
            ok: false,
            latencyMs: round(now() - t),
            transport: "unknown",
            detail: `fetch failed: ${e}`,
          };
        }
        const latencyMs = round(now() - t);
        const transport = await detectTransport(node, peer.nodeId);
        const fetchOk = status != null && status < 500;
        // F8: this case only proves relay fallback if the measured transport is
        // actually "relay". Because a prior direct-dial in the same run may have
        // warmed a pooled direct connection that this fetch reuses, omitting
        // directAddrs does NOT guarantee relay routing. A direct/unknown result
        // is therefore inconclusive — skip rather than falsely pass a relay
        // claim. (A truly relay-only client — fresh node, direct discovery
        // disabled — is tracked as a follow-up; the single shared harness node
        // cannot isolate it.)
        if (fetchOk && transport !== "relay") {
          return {
            ok: false,
            skip: true,
            latencyMs,
            transport,
            detail:
              `HTTP ${status} — transport=${transport}; relay not exercised ` +
              "(reused/available direct path — cannot confirm relay fallback in a shared-node harness)",
          };
        }
        return {
          ok: fetchOk && transport === "relay",
          latencyMs,
          transport,
          detail: `HTTP ${status} — transport=${transport}` +
            (transport === "relay" ? " (relay ✓)" : ""),
        };
      },
    },
  ];

  // ── http-compliance ────────────────────────────────────────────────────────
  function complianceTest(tc, againstSelf = false) {
    const targetLabel = againstSelf ? "self" : "peer";
    return {
      id: `http-compliance-${tc.id}`,
      group: "http-compliance",
      name: `${tc.id}${
        tc.description ? ` — ${tc.description}` : ""
      } (${targetLabel})`,
      async run() {
        if (!againstSelf && !peer) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail: "no peer selected",
          };
        }
        const serverId = againstSelf ? self.nodeId : peer.nodeId;
        const t = now();
        let caseResult = null;
        try {
          await runCases({
            fetch: (url, init) => fetch(url, init),
            serverId,
            directAddrs: againstSelf || !peerDirect.length
              ? undefined
              : peerDirect,
            cases: [tc],
            onCaseResult: (r) => {
              caseResult = r;
            },
          });
        } catch (e) {
          return {
            ok: false,
            latencyMs: round(now() - t),
            detail: `run error: ${e}`,
          };
        }
        const latencyMs = caseResult
          ? round(caseResult.latencyMs)
          : round(now() - t);
        return {
          ok: !!caseResult && caseResult.ok,
          latencyMs,
          detail: caseResult
            ? caseResult.ok
              ? `HTTP ${caseResult.status ?? "—"} ✓`
              : caseResult.error ?? "failed"
            : "no result",
        };
      },
    };
  }

  const complianceTests = [
    complianceTest(selfLoopbackCase, true),
    ...(cases ?? [])
      .filter((c) => c && c.id && !c.skip)
      .map((c) => complianceTest(c)),
  ];

  // ── serve-stop ─────────────────────────────────────────────────────────────
  const serveStopTests = [
    {
      id: "serve-stop-lifecycle",
      group: "serve-stop",
      name: "serve → reachable → stop → refused",
      async run() {
        if (isServing) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail:
              "a local service is already bound (testing mode). Disable testing mode to run the standalone serve→stop lifecycle.",
          };
        }
        if (!ctx.handler) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail: "no handler provided",
          };
        }
        const t = now();
        const ac = new AbortController();
        let reachable = false;
        let refused = false;
        let handle = null;
        try {
          handle = node.serve({ signal: ac.signal }, ctx.handler);
          const res = await fetch(`httpi://${self.nodeId}/hello`, {
            method: "GET",
          });
          reachable = res.status < 500;
          await res.arrayBuffer();
        } catch (e) {
          ac.abort();
          return {
            ok: false,
            latencyMs: round(now() - t),
            detail: `serve/reach failed: ${e}`,
          };
        }
        // F20: deterministically await the serve loop's own shutdown contract
        // (ServeHandle.finished) instead of sleeping a fixed 300ms and hoping the
        // loop has drained. Bound the wait so a stuck stop can't hang the suite.
        ac.abort();
        try {
          const finished = handle?.finished ?? Promise.resolve();
          await Promise.race([
            finished,
            new Promise((_r, reject) =>
              setTimeout(() => reject(new Error("serve stop timed out")), 5000)
            ),
          ]);
        } catch {
          /* fall through: probe reachability regardless */
        }
        try {
          await fetch(`httpi://${self.nodeId}/hello`, { method: "GET" });
        } catch {
          refused = true;
        }
        const latencyMs = round(now() - t);
        return {
          ok: reachable && refused,
          latencyMs,
          detail:
            `reachable-while-serving=${reachable} refused-after-stop=${refused}`,
        };
      },
    },
  ];

  return [
    { id: "discovery", name: "Discovery", tests: discoveryTests },
    { id: "direct-dial", name: "Direct dial", tests: directDialTests },
    { id: "relay-fallback", name: "Relay fallback", tests: relayTests },
    { id: "http-compliance", name: "HTTP compliance", tests: complianceTests },
    { id: "serve-stop", name: "Serve lifecycle", tests: serveStopTests },
  ];
}

/**
 * Execute a built suite, invoking `onResult` after each test. Emits a stable,
 * greppable `IROH_INTEROP_CASE …` line per test (mirrors `IROH_DNSSD_CHECK`).
 *
 * @returns {Promise<{ groups: Array, results: Array, summary: object }>}
 */
export async function runSuite(
  groups,
  { onResult, log, deadlineMs = 20000 } = {},
) {
  let pass = 0;
  let fail = 0;
  let skip = 0;
  let direct = 0;
  let relay = 0;
  let transportTotal = 0;
  const flat = [];

  for (const group of groups) {
    for (const test of group.tests) {
      onResult?.({ phase: "start", group: group.id, test });
      let res;
      const startedAt = now();
      // F20: bound every case so a hung fetch/browse can't stall the suite.
      // Track the deadline timer so it can be cleared the instant the case
      // settles — otherwise a long default deadline keeps the event loop (and
      // any headless test runner) alive well past the last result.
      let deadlineTimer;
      try {
        res = await Promise.race([
          test.run(),
          new Promise((_r, reject) => {
            deadlineTimer = setTimeout(
              () => reject(new Error(`case exceeded ${deadlineMs}ms deadline`)),
              deadlineMs,
            );
          }),
        ]);
      } catch (e) {
        res = {
          ok: false,
          latencyMs: round(now() - startedAt),
          detail: `threw: ${e}`,
        };
      } finally {
        clearTimeout(deadlineTimer);
      }
      const outcome = res.skip ? "skip" : res.ok ? "pass" : "fail";
      if (outcome === "pass") pass += 1;
      else if (outcome === "fail") fail += 1;
      else skip += 1;
      // F27: only tests that actually measured a transport contribute to the
      // transport rollup — compliance/serve-stop/discovery cases carry no
      // transport and must not inflate the "unknown" bucket.
      if (typeof res.transport === "string") {
        transportTotal += 1;
        if (res.transport === "direct") direct += 1;
        else if (res.transport === "relay") relay += 1;
      }

      const record = {
        id: test.id,
        group: group.id,
        name: test.name,
        outcome,
        ...res,
      };
      flat.push(record);

      const fields = {
        id: test.id,
        group: group.id,
        outcome,
        latencyMs: res.latencyMs ?? 0,
      };
      if (res.transport) fields.transport = res.transport;
      const line = `${IROH_INTEROP_CASE} ` +
        Object.entries(fields).map(([k, v]) => `${k}=${v}`).join(" ");
      if (log) log(line);
      else if (typeof console !== "undefined") console.log(line);

      onResult?.({ phase: "result", group: group.id, test, result: record });
    }
  }

  return {
    groups,
    results: flat,
    summary: {
      total: flat.length,
      pass,
      fail,
      skip,
      transport: { direct, relay, unknown: transportTotal - direct - relay },
    },
  };
}
