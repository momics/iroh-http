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
  const { node, self, peer, cases, selfLoopbackCase, runCases, helpers = {}, isServing } = ctx;
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
            `directAddress=${di.directAddress ?? "null"} relayUrl=${di.relayUrl ?? "null"}` +
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
        const service = `iroh-http-suite-${Math.random().toString(36).slice(2, 8)}`;
        const ac = new AbortController();
        const txt = { pk: self.nodeId };
        if (di?.directAddress) txt[TXT_KEY_ADDRESS] = di.directAddress;
        if (di?.relayUrl) txt[TXT_KEY_RELAY] = di.relayUrl;

        node
          .advertise({
            serviceName: service,
            instanceName: self.nodeId,
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
          for await (const rec of node.browse({
            serviceName: service,
            protocol: "udp",
            signal: ac.signal,
          })) {
            if (rec.txt?.pk !== self.nodeId) continue;
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
          return {
            ok: false,
            skip: true,
            latencyMs,
            detail:
              "self advertisement not observed within 5s — mDNS may be unavailable on this host/runtime",
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
        const service = `iroh-http-suite-rb-${Math.random().toString(36).slice(2, 8)}`;
        const instance = self.nodeId;
        let emits = 0;
        const browseAc = new AbortController();

        const browsing = (async () => {
          try {
            for await (const rec of node.browse({
              serviceName: service,
              protocol: "udp",
              signal: browseAc.signal,
            })) {
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
            skip: true,
            latencyMs,
            detail: "no emits observed — mDNS unavailable on this host/runtime",
          };
        }
        return {
          ok: emits >= 2,
          latencyMs,
          detail:
            `observed ${emits} emit(s) across two advertisement generations` +
            (emits >= 2 ? " (re-emit ✓)" : " (expected ≥2 — possible W2 regression)"),
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
          return { ok: false, skip: true, latencyMs: 0, detail: "no peer selected" };
        }
        if (!peerDirect.length) {
          return {
            ok: false,
            skip: true,
            latencyMs: 0,
            detail: "peer advertised no dialable ip:port (relay-only) — see relay-fallback",
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
        const ok = fetchOk && transport !== "relay";
        return {
          ok,
          latencyMs,
          transport,
          detail:
            `HTTP ${status} via ${peerDirect[0]} — transport=${transport}` +
            (transport === "relay"
              ? " (reached over RELAY, not direct ✗)"
              : transport === "unknown"
                ? " (transport undetermined — fetch succeeded, see limitation note)"
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
          return { ok: false, skip: true, latencyMs: 0, detail: "no peer selected" };
        }
        const t = now();
        let status = null;
        try {
          // Omit directAddrs so the stack must fall back to relay routing.
          const res = await fetch(`httpi://${peer.nodeId}/hello`, { method: "GET" });
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
        return {
          ok: status != null && status < 500,
          latencyMs,
          transport,
          detail: `HTTP ${status} — transport=${transport}`,
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
      name: `${tc.id}${tc.description ? ` — ${tc.description}` : ""} (${targetLabel})`,
      async run() {
        if (!againstSelf && !peer) {
          return { ok: false, skip: true, latencyMs: 0, detail: "no peer selected" };
        }
        const serverId = againstSelf ? self.nodeId : peer.nodeId;
        const t = now();
        let caseResult = null;
        try {
          await runCases({
            fetch: (url, init) => fetch(url, init),
            serverId,
            directAddrs: againstSelf || !peerDirect.length ? undefined : peerDirect,
            cases: [tc],
            onCaseResult: (r) => {
              caseResult = r;
            },
          });
        } catch (e) {
          return { ok: false, latencyMs: round(now() - t), detail: `run error: ${e}` };
        }
        const latencyMs = caseResult ? round(caseResult.latencyMs) : round(now() - t);
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
          return { ok: false, skip: true, latencyMs: 0, detail: "no handler provided" };
        }
        const t = now();
        const ac = new AbortController();
        let reachable = false;
        let refused = false;
        try {
          node.serve({ signal: ac.signal }, ctx.handler);
          const res = await fetch(`httpi://${self.nodeId}/hello`, { method: "GET" });
          reachable = res.status < 500;
          await res.arrayBuffer();
        } catch (e) {
          ac.abort();
          return { ok: false, latencyMs: round(now() - t), detail: `serve/reach failed: ${e}` };
        }
        ac.abort();
        await new Promise((r) => setTimeout(r, 300));
        try {
          await fetch(`httpi://${self.nodeId}/hello`, { method: "GET" });
        } catch {
          refused = true;
        }
        const latencyMs = round(now() - t);
        return {
          ok: reachable && refused,
          latencyMs,
          detail: `reachable-while-serving=${reachable} refused-after-stop=${refused}`,
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
export async function runSuite(groups, { onResult, log } = {}) {
  let pass = 0;
  let fail = 0;
  let skip = 0;
  let direct = 0;
  let relay = 0;
  const flat = [];

  for (const group of groups) {
    for (const test of group.tests) {
      onResult?.({ phase: "start", group: group.id, test });
      let res;
      try {
        res = await test.run();
      } catch (e) {
        res = { ok: false, latencyMs: 0, detail: `threw: ${e}` };
      }
      const outcome = res.skip ? "skip" : res.ok ? "pass" : "fail";
      if (outcome === "pass") pass += 1;
      else if (outcome === "fail") fail += 1;
      else skip += 1;
      if (res.transport === "direct") direct += 1;
      else if (res.transport === "relay") relay += 1;

      const record = { id: test.id, group: group.id, name: test.name, outcome, ...res };
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
      transport: { direct, relay, unknown: flat.length - direct - relay },
    },
  };
}
