/**
 * iroh-http Tauri — comprehensive dev console.
 *
 * Covers all major features of the iroh-http-tauri plugin:
 *   - Identity & address introspection
 *   - Key persistence across restarts
 *   - HTTP serve & fetch (with WebTransport echo)
 *   - Peer address info & connection stats
 *   - mDNS discovery (browse + advertise)
 *   - Generic DNS-SD (advertise/browse any service)
 *   - QUIC sessions (bidi streams + datagrams)
 *   - Ed25519 sign, verify, and key generation
 */
import {
  asIrohPeer,
  createNode,
  isDialableSocketAddr,
  PublicKey,
  SecretKey,
  TXT_KEY_ADDRESS,
  TXT_KEY_RELAY,
} from "@momics/iroh-http-tauri";
import {
  type IrohSession,
  isRelayUrl,
  type NodeOptions,
} from "@momics/iroh-http-shared";
// @ts-ignore — .mjs from the shared cross-runtime compliance suite (pure WHATWG).
import { handleRequest as handleComplianceRequest } from "../../../tests/http-compliance/handler.mjs";
// @ts-ignore — .mjs from the shared compliance suite (pure WHATWG).
import { runCases as runComplianceCases } from "../../../tests/http-compliance/harness.mjs";
import complianceCases from "../../../tests/http-compliance/cases.json";
// @ts-ignore — .mjs from the shared interop suite (pure WHATWG).
import { buildSuite, runSuite } from "../../../tests/interop/suite.mjs";
import { getPeers, type RegistryPeer, upsertPeer } from "./peer-registry.js";
import { createPeerPicker } from "./peer-picker.js";

// ── Utilities ──────────────────────────────────────────────────────────────────

function appendLog(el: HTMLElement, msg: string): void {
  const ts = new Date().toLocaleTimeString();
  el.textContent = el.textContent
    ? `${el.textContent}\n[${ts}] ${msg}`
    : `[${ts}] ${msg}`;
  el.scrollTop = el.scrollHeight;
}

function setStatus(
  el: HTMLElement,
  msg: string,
  kind: "ok" | "error" | "" = "",
): void {
  el.textContent = msg;
  el.className = kind ? `status ${kind}` : "status";
}

/** Split mixed registry addresses into the two independent dialing hints. */
function dialHints(addrs: string[]): {
  directAddrs?: string[];
  relayUrl?: string;
} {
  const directAddrs = addrs.filter(isDialableSocketAddr);
  const relayUrl = addrs.find(isRelayUrl);
  return {
    ...(directAddrs.length ? { directAddrs } : {}),
    ...(relayUrl ? { relayUrl } : {}),
  };
}

// Stable, greppable structured log prefix for the on-device DNS-SD checks
// (#334). Lines reach `adb logcat` / iOS device logs via the webview→tracing
// wiring, and are mirrored into a visible <pre> so the operator can read them
// without a cable. Format: `IROH_DNSSD_CHECK <k>=<v> …`. Keep it single-line and
// space-separated so it stays trivially greppable.
const IROH_DNSSD_CHECK = "IROH_DNSSD_CHECK";

function dnssdCheck(
  logEl: HTMLElement | null,
  fields: Record<string, string | number | boolean>,
): void {
  const line = `${IROH_DNSSD_CHECK} ` +
    Object.entries(fields)
      .map(([k, v]) => `${k}=${v}`)
      .join(" ");
  console.log(line);
  if (logEl) appendLog(logEl, line);
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function fromHex(hex: string): Uint8Array {
  const pairs = hex.replace(/\s/g, "").match(/.{2}/g) ?? [];
  return new Uint8Array(pairs.map((h) => parseInt(h, 16)));
}

const te = new TextEncoder();
const td = new TextDecoder();

// ── Key persistence ────────────────────────────────────────────────────────────
//
// DEMO ONLY — INSECURE AT REST. localStorage is plaintext, readable by any code
// running in the webview and by anyone with disk access. A real app MUST store
// the node secret key in OS-backed secure storage (e.g. tauri-plugin-stronghold
// or the OS keychain). See SECURITY.md for the key-at-rest guidance.

const KEY_STORAGE = "iroh-http-demo-key-v1";

// Access-control allowlist for the demo server. Add authenticated peer node IDs
// here to restrict who may be served; empty = serve anyone (demo only).
const ALLOWED_PEERS = new Set<string>();

function loadSavedKey(): Uint8Array | undefined {
  const stored = localStorage.getItem(KEY_STORAGE);
  if (!stored) return undefined;
  try {
    return Uint8Array.from(atob(stored), (c) => c.charCodeAt(0));
  } catch {
    return undefined;
  }
}

function persistKey(key: Uint8Array): void {
  localStorage.setItem(KEY_STORAGE, btoa(String.fromCharCode(...key)));
}

// ── Node init ──────────────────────────────────────────────────────────────────

const savedKey = loadSavedKey();
// The app owns its identity: reuse the saved key, or generate one ourselves.
// A key is always supplied to createNode, so node.secretKey is guaranteed
// present (the Tauri adapter never exports a native-generated key).
const key = savedKey ?? SecretKey.generate().toBytes();
const node = await createNode({ key });
// Always persist so a just-generated key survives the next restart.
persistKey(node.secretKey.toBytes());

// ── Tab navigation ─────────────────────────────────────────────────────────────

{
  const btns = Array.from(
    document.querySelectorAll<HTMLButtonElement>("[data-tab]"),
  );
  const panels = Array.from(
    document.querySelectorAll<HTMLElement>("[data-panel]"),
  );
  btns.forEach((btn) => {
    btn.addEventListener("click", () => {
      const target = btn.dataset.tab!;
      btns.forEach((b) =>
        b.classList.toggle("active", b.dataset.tab === target)
      );
      panels.forEach((p) =>
        p.classList.toggle("active", p.dataset.panel === target)
      );
    });
  });
}

// ── Keyboard-aware viewport (mobile) ─────────────────────────────────────────
//
// Tauri renders edge-to-edge on Android, so the native soft keyboard slides
// over the webview instead of resizing it — a focused input near the bottom
// (e.g. the peer node-id field) ends up hidden behind the keyboard and can't be
// pasted into. The `visualViewport` API reports the region left visible above
// the keyboard; pad the body by the keyboard's height so the page can scroll
// far enough, and pull the focused field up into view. Works on iOS too.

{
  const vv = window.visualViewport;
  if (vv) {
    const isTextField = (node: EventTarget | null): node is HTMLElement =>
      node instanceof HTMLElement &&
      (node.tagName === "INPUT" || node.tagName === "TEXTAREA");

    const keyboardHeight = (): number =>
      Math.max(0, window.innerHeight - vv.height - vv.offsetTop);

    const syncInset = (): void => {
      const kb = keyboardHeight();
      document.body.style.paddingBottom = kb > 0 ? `${kb}px` : "";
      if (kb > 0 && isTextField(document.activeElement)) {
        document.activeElement.scrollIntoView({ block: "center" });
      }
    };

    vv.addEventListener("resize", syncInset);
    vv.addEventListener("scroll", syncInset);

    document.addEventListener("focusin", (e) => {
      if (!isTextField(e.target)) return;
      // Wait for the keyboard to animate in and update `visualViewport`.
      window.setTimeout(() => {
        syncInset();
        (e.target as HTMLElement).scrollIntoView({ block: "center" });
      }, 300);
    });

    document.addEventListener("focusout", () => {
      document.body.style.paddingBottom = "";
    });
  }
}

// ── Identity ───────────────────────────────────────────────────────────────────

async function copyText(
  btn: HTMLButtonElement,
  getText: () => string | Promise<string>,
): Promise<void> {
  const text = await getText();
  await navigator.clipboard.writeText(text);
  const prev = btn.textContent!;
  btn.textContent = "Copied!";
  setTimeout(() => (btn.textContent = prev), 1500);
}

// Mount the reusable, registry-backed peer-picker into a container. On select,
// `onPick` fills that tab's target field so a peer discovered anywhere can be
// targeted here without pasting an id.
function mountPeerPicker(
  mountId: string,
  onPick: (peer: RegistryPeer) => void,
  opts?: { placeholder?: string; compact?: boolean },
): void {
  const mount = document.querySelector<HTMLElement>(`#${mountId}`);
  if (!mount) return;
  const picker = createPeerPicker({
    onSelect: onPick,
    placeholder: opts?.placeholder,
    compact: opts?.compact,
  });
  mount.append(picker.element);
}

const nodeIdEl = document.querySelector<HTMLElement>("#node-id")!;
const copyIdBtn = document.querySelector<HTMLButtonElement>("#copy-id-btn")!;
const ticketEl = document.querySelector<HTMLElement>("#node-ticket")!;
const copyTicketBtn = document.querySelector<HTMLButtonElement>(
  "#copy-ticket-btn",
)!;
const relayEl = document.querySelector<HTMLElement>("#home-relay")!;
const addrEl = document.querySelector<HTMLElement>("#node-addr")!;
const keyStatusEl = document.querySelector<HTMLElement>("#key-status")!;

nodeIdEl.textContent = node.publicKey.toString();
copyIdBtn.addEventListener(
  "click",
  () => copyText(copyIdBtn, () => node.publicKey.toString()),
);
copyTicketBtn.addEventListener(
  "click",
  () => copyText(copyTicketBtn, () => node.ticket()),
);

setStatus(
  keyStatusEl,
  savedKey ? "Loaded saved key ✓" : "Generated new key (not yet saved)",
);

document.querySelector<HTMLButtonElement>("#save-key-btn")!.addEventListener(
  "click",
  () => {
    persistKey(node.secretKey.toBytes());
    setStatus(
      keyStatusEl,
      "Key saved to localStorage (insecure — demo only) ⚠",
      "ok",
    );
  },
);

document.querySelector<HTMLButtonElement>("#clear-key-btn")!.addEventListener(
  "click",
  () => {
    localStorage.removeItem(KEY_STORAGE);
    setStatus(keyStatusEl, "Cleared — next launch generates a new identity");
  },
);

void (async () => {
  try {
    const [ticket, relay, addr] = await Promise.all([
      node.ticket(),
      node.homeRelay(),
      node.addr(),
    ]);
    ticketEl.textContent = ticket;
    relayEl.textContent = relay ?? "(no relay — direct connections only)";
    addrEl.textContent = JSON.stringify(addr, null, 2);
  } catch (e) {
    console.error("Identity load:", e);
  }
})();

// ── HTTP server ────────────────────────────────────────────────────────────────

// Bundled demo media this node hosts over httpi:// (see the serve handler).
// Mirrors the files the Deno `host` task serves, so the Stream-files tab works
// against this node — including against its own ID (self-request loopback).
const HOSTED_FILES = new Set(["/audio.wav", "/image.png"]);

const serveBtn = document.querySelector<HTMLButtonElement>("#serve-btn")!;
const serverStatus = document.querySelector<HTMLElement>("#server-status")!;
const serverLog = document.querySelector<HTMLElement>("#server-log")!;

let serveAbort: AbortController | null = null;

serveBtn.addEventListener("click", () => {
  if (serveAbort) {
    serveAbort.abort();
    serveAbort = null;
    serveBtn.textContent = "Start serving";
    setStatus(serverStatus, "Stopped");
    return;
  }

  serveAbort = new AbortController();
  node.serve({ signal: serveAbort.signal }, async (req) => {
    // Access control: only peers in ALLOWED_PEERS may be served. The `peer-id`
    // header is the authenticated QUIC identity set by the library, not
    // client-controlled. Empty by default = serves anyone (demo only — never
    // ship a real app open like this).
    const peerId = req.headers.get("peer-id") ?? "";
    if (ALLOWED_PEERS.size > 0 && !ALLOWED_PEERS.has(peerId)) {
      appendLog(
        serverLog,
        `Rejected non-allowlisted peer ${peerId.slice(0, 20)}…`,
      );
      return new Response("Forbidden", { status: 403 });
    }
    // WebTransport bidi stream (from session.createBidirectionalStream on remote peer).
    const wt = (req as unknown as {
      acceptWebTransport?: () => {
        readable: ReadableStream<Uint8Array>;
        writable: WritableStream<Uint8Array>;
      } | null;
    }).acceptWebTransport?.();
    if (wt) {
      const peer = req.headers.get("peer-id") ?? "?";
      appendLog(
        serverLog,
        `WebTransport from ${peer.slice(0, 20)}… (echo mode)`,
      );
      void (async () => {
        const reader = wt.readable.getReader();
        const writer = wt.writable.getWriter();
        try {
          while (true) {
            const { value, done } = await reader.read();
            if (done) break;
            const text = td.decode(value);
            appendLog(serverLog, `  ← wt: "${text}" → echoing`);
            await writer.write(te.encode(text));
          }
        } catch { /* connection closed */ }
        writer.close().catch(() => {});
      })();
      return new Response(null, { status: 200 });
    }

    const path = new URL(req.url).pathname;
    const peer = req.headers.get("peer-id") ?? "?";
    appendLog(serverLog, `${req.method} ${path} ← ${peer.slice(0, 20)}…`);

    // Host the bundled demo media (audio.wav, image.png) so this node can serve
    // files to peers — and to itself. Pointing the Stream-files tab at this
    // node's own ID exercises the in-process self-request loopback (a node
    // fetching its own ID, routed straight into this handler). The webview
    // fetches the bundled asset from the app origin and forwards it, honoring
    // Range so <audio> can seek.
    if (HOSTED_FILES.has(path)) {
      const range = req.headers.get("range");
      const asset = await fetch(path, range ? { headers: { range } } : {});
      const headers = new Headers();
      for (
        const h of [
          "content-type",
          "content-length",
          "content-range",
          "accept-ranges",
          "etag",
          "last-modified",
        ]
      ) {
        const v = asset.headers.get(h);
        if (v) headers.set(h, v);
      }
      return new Response(asset.body, { status: asset.status, headers });
    }

    return new Response(`Hello from iroh-http! (${req.method} ${path})`, {
      headers: { "content-type": "text/plain" },
    });
  });

  serveBtn.textContent = "Stop serving";
  setStatus(serverStatus, "Listening", "ok");
});

// ── HTTP client ────────────────────────────────────────────────────────────────

const fetchForm = document.querySelector<HTMLFormElement>("#fetch-form")!;
const peerInput = document.querySelector<HTMLInputElement>("#peer-input")!;
const pathInput = document.querySelector<HTMLInputElement>("#path-input")!;
const methodSelect = document.querySelector<HTMLSelectElement>(
  "#method-select",
)!;
const responseStatus = document.querySelector<HTMLElement>("#response-status")!;
const responseBody = document.querySelector<HTMLElement>("#response-body")!;

fetchForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const peer = peerInput.value.trim();
  const path = pathInput.value.trim() || "/";
  if (!peer) {
    peerInput.focus();
    return;
  }

  setStatus(responseStatus, "fetching…");
  responseBody.textContent = "";
  // A manually-entered target counts as a discovered peer for the registry so
  // it can be re-targeted from other tabs via the picker.
  if (/^[0-9a-z]{40,}$/i.test(peer)) {
    upsertPeer({ nodeId: peer, source: "manual" });
  }

  try {
    const known = getPeers().find((candidate) => candidate.nodeId === peer);
    const res = await node.fetch(`httpi://${peer}${path}`, {
      method: methodSelect.value,
      ...dialHints(known?.addrs ?? []),
    });
    setStatus(responseStatus, `HTTP ${res.status}`, res.ok ? "ok" : "error");
    responseBody.textContent = await res.text();
  } catch (e) {
    setStatus(responseStatus, "error", "error");
    responseBody.textContent = String(e);
  }
});

mountPeerPicker("http-peer-picker", (p) => {
  peerInput.value = p.nodeId;
  peerInput.focus();
});

// ── Stream files (httpi:// scheme) ───────────────────────────────────────────────

const filesHostInput = document.querySelector<HTMLInputElement>(
  "#files-host-input",
)!;
const filesStatus = document.querySelector<HTMLElement>("#files-status")!;
const filesAudioPath = document.querySelector<HTMLInputElement>(
  "#files-audio-path",
)!;
const filesAudio = document.querySelector<HTMLAudioElement>("#files-audio")!;
const filesImagePath = document.querySelector<HTMLInputElement>(
  "#files-image-path",
)!;
const filesImage = document.querySelector<HTMLImageElement>("#files-image")!;

/** Build an `httpi://<host><path>` URL from the host input and a path field. */
function filesUrl(rawPath: string, fallback: string): string | null {
  const host = filesHostInput.value.trim();
  if (!host) {
    filesHostInput.focus();
    return null;
  }
  const p = rawPath.trim() || fallback;
  if (/^[0-9a-z]{40,}$/i.test(host)) {
    upsertPeer({ nodeId: host, source: "manual" });
  }
  return `httpi://${host}${p.startsWith("/") ? p : `/${p}`}`;
}

mountPeerPicker("files-peer-picker", (p) => {
  filesHostInput.value = p.nodeId;
});

// Fill the host field with this node's own ID so the Stream-files tab targets
// the local server — exercising the self-request loopback. Requires serving
// (Server tab → Start serving) so this node has a handler for its own ID.
document.querySelector<HTMLButtonElement>("#files-use-self-btn")!
  .addEventListener(
    "click",
    () => {
      filesHostInput.value = node.publicKey.toString();
      setStatus(
        filesStatus,
        serveAbort
          ? "targeting this node (self-request loopback)"
          : "targeting this node — start serving first (Server tab)",
        serveAbort ? "ok" : "error",
      );
    },
  );

document.querySelector<HTMLButtonElement>("#files-audio-btn")!.addEventListener(
  "click",
  () => {
    const src = filesUrl(filesAudioPath.value, "/audio.wav");
    if (!src) return;
    // Point the native <audio> element straight at the host. The httpi:// scheme
    // handler resolves the request through iroh-http-core over QUIC — no IPC, no
    // base64, and the webview can issue Range requests to seek.
    setStatus(filesStatus, `loading ${src}`);
    filesAudio.src = src;
    filesAudio.play().then(
      () => setStatus(filesStatus, "playing audio ▶", "ok"),
      (e) => setStatus(filesStatus, `playback blocked: ${e}`, "error"),
    );
  },
);

document.querySelector<HTMLButtonElement>("#files-image-btn")!.addEventListener(
  "click",
  () => {
    const src = filesUrl(filesImagePath.value, "/image.png");
    if (!src) return;
    setStatus(filesStatus, `loading ${src}`);
    filesImage.src = src;
  },
);

filesAudio.addEventListener("error", () => {
  const err = filesAudio.error;
  setStatus(
    filesStatus,
    `audio error${
      err ? ` (code ${err.code})` : ""
    } — is the host running and reachable?`,
    "error",
  );
});
filesAudio.addEventListener(
  "ended",
  () => setStatus(filesStatus, "audio ended"),
);
filesImage.addEventListener("load", () => {
  filesImage.style.display = "block";
  setStatus(filesStatus, "image loaded ✓", "ok");
});
filesImage.addEventListener("error", () => {
  filesImage.style.display = "none";
  setStatus(
    filesStatus,
    "image error — is the host running and reachable?",
    "error",
  );
});

// ── Peer info & stats ──────────────────────────────────────────────────────────

const peerLookupInput = document.querySelector<HTMLInputElement>(
  "#peer-lookup-input",
)!;
const peerInfoOutput = document.querySelector<HTMLElement>(
  "#peer-info-output",
)!;

mountPeerPicker("peer-peer-picker", (p) => {
  peerLookupInput.value = p.nodeId;
});

document.querySelector<HTMLButtonElement>("#peer-info-btn")!.addEventListener(
  "click",
  async () => {
    const peer = peerLookupInput.value.trim();
    if (!peer) {
      peerLookupInput.focus();
      return;
    }
    peerInfoOutput.textContent = "loading…";
    try {
      const info = await node.peerInfo(peer);
      peerInfoOutput.textContent = info
        ? JSON.stringify(info, null, 2)
        : "(peer not yet seen — connect to it first via HTTP or Sessions)";
    } catch (e) {
      peerInfoOutput.textContent = String(e);
    }
  },
);

document.querySelector<HTMLButtonElement>("#peer-stats-btn")!.addEventListener(
  "click",
  async () => {
    const peer = peerLookupInput.value.trim();
    if (!peer) {
      peerLookupInput.focus();
      return;
    }
    peerInfoOutput.textContent = "loading…";
    try {
      const stats = await node.peerStats(peer);
      peerInfoOutput.textContent = stats
        ? JSON.stringify(stats, null, 2)
        : "(no stats — establish a QUIC connection first)";
    } catch (e) {
      peerInfoOutput.textContent = String(e);
    }
  },
);

// ── Discovery (mDNS) ───────────────────────────────────────────────────────────

const advertisePeerBtn = document.querySelector<HTMLButtonElement>(
  "#advertise-peer-btn",
)!;
const advertisePeerStatus = document.querySelector<HTMLElement>(
  "#advertise-peer-status",
)!;
const advertisePeerServiceInput = document.querySelector<HTMLInputElement>(
  "#advertise-peer-service",
)!;

const browsePeersBtn = document.querySelector<HTMLButtonElement>(
  "#browse-peers-btn",
)!;
const browsePeersLog = document.querySelector<HTMLElement>(
  "#browse-peers-log",
)!;
const browsePeersServiceInput = document.querySelector<HTMLInputElement>(
  "#browse-peers-service",
)!;
const browsePeersList = document.querySelector<HTMLUListElement>(
  "#browse-peers-list",
)!;
const browsePeersEmpty = document.querySelector<HTMLElement>(
  "#browse-peers-empty",
)!;

let advertisePeerAbort: AbortController | null = null;
let browsePeersAbort: AbortController | null = null;

// Live view of currently-discovered peers, keyed by node id. `browsePeers`
// yields an event per peer coming (isActive) or going (!isActive); we add a row
// on arrival and remove it on departure so the list always reflects who's
// reachable right now. Each row copies the full node id — paste it into the
// HTTP tab's peer field to fetch from that peer.
const discoveredPeers = new Map<string, HTMLLIElement>();

function refreshPeerEmptyState(): void {
  browsePeersEmpty.classList.toggle("hidden", discoveredPeers.size > 0);
}

function addDiscoveredPeer(nodeId: string): void {
  if (discoveredPeers.has(nodeId)) return;

  const li = document.createElement("li");
  li.className = "peer-item";
  li.dataset.nodeId = nodeId;

  const code = document.createElement("code");
  code.className = "peer-id";
  code.textContent = `${nodeId.slice(0, 16)}…`;
  code.title = nodeId;

  const copyBtn = document.createElement("button");
  copyBtn.className = "btn-secondary";
  copyBtn.textContent = "Copy";
  copyBtn.addEventListener("click", () => copyText(copyBtn, () => nodeId));

  li.append(code, copyBtn);
  browsePeersList.append(li);
  discoveredPeers.set(nodeId, li);
  refreshPeerEmptyState();
}

function removeDiscoveredPeer(nodeId: string): void {
  const li = discoveredPeers.get(nodeId);
  if (!li) return;
  li.remove();
  discoveredPeers.delete(nodeId);
  refreshPeerEmptyState();
}

function clearDiscoveredPeers(): void {
  discoveredPeers.clear();
  browsePeersList.replaceChildren();
  refreshPeerEmptyState();
}

advertisePeerBtn.addEventListener("click", async () => {
  if (advertisePeerAbort) {
    advertisePeerAbort.abort();
    advertisePeerAbort = null;
    advertisePeerBtn.textContent = "Start advertising";
    setStatus(advertisePeerStatus, "Stopped");
    return;
  }

  const serviceName = advertisePeerServiceInput.value.trim() || "iroh-http";
  advertisePeerAbort = new AbortController();
  advertisePeerBtn.textContent = "Stop advertising";
  setStatus(advertisePeerStatus, `Advertising as "${serviceName}"`, "ok");

  try {
    await node.advertisePeer({
      serviceName,
      signal: advertisePeerAbort.signal,
    });
  } catch { /* aborted or error */ }

  advertisePeerAbort = null;
  advertisePeerBtn.textContent = "Start advertising";
  setStatus(advertisePeerStatus, "Done");
});

browsePeersBtn.addEventListener("click", async () => {
  if (browsePeersAbort) {
    browsePeersAbort.abort();
    browsePeersAbort = null;
    browsePeersBtn.textContent = "Start browsing";
    clearDiscoveredPeers();
    return;
  }

  const serviceName = browsePeersServiceInput.value.trim() || "iroh-http";
  browsePeersAbort = new AbortController();
  browsePeersBtn.textContent = "Stop browsing";
  browsePeersLog.textContent = "";
  clearDiscoveredPeers();

  try {
    for await (
      const ev of node.browsePeers({
        serviceName,
        signal: browsePeersAbort.signal,
      })
    ) {
      if (ev.isActive) addDiscoveredPeer(ev.nodeId);
      else removeDiscoveredPeer(ev.nodeId);

      if (ev.isActive) {
        upsertPeer({
          nodeId: ev.nodeId,
          source: "discovery",
          addrs: ev.addrs,
        });
      }

      const icon = ev.isActive ? "+" : "-";
      appendLog(
        browsePeersLog,
        `${icon} ${ev.nodeId.slice(0, 20)}… [${ev.addrs.join(", ")}]`,
      );
    }
  } catch (e) {
    appendLog(browsePeersLog, `Error: ${e}`);
  }

  browsePeersAbort = null;
  browsePeersBtn.textContent = "Start browsing";
  clearDiscoveredPeers();
});

// ── Generic DNS-SD ─────────────────────────────────────────────────────────────

const genericServiceInput = document.querySelector<HTMLInputElement>(
  "#generic-service",
)!;
const genericAdvertiseBtn = document.querySelector<HTMLButtonElement>(
  "#generic-advertise-btn",
)!;
const genericBrowseBtn = document.querySelector<HTMLButtonElement>(
  "#generic-browse-btn",
)!;
const genericStatus = document.querySelector<HTMLElement>("#generic-status")!;
const genericLog = document.querySelector<HTMLElement>("#generic-log")!;

let genericAdvertiseAbort: AbortController | null = null;
let genericBrowseAbort: AbortController | null = null;

genericAdvertiseBtn.addEventListener("click", async () => {
  if (genericAdvertiseAbort) {
    genericAdvertiseAbort.abort();
    genericAdvertiseAbort = null;
    genericAdvertiseBtn.textContent = "Start advertising";
    setStatus(genericStatus, "Stopped");
    return;
  }

  const serviceName = genericServiceInput.value.trim() || "demo-printer";
  genericAdvertiseAbort = new AbortController();
  genericAdvertiseBtn.textContent = "Stop advertising";
  setStatus(
    genericStatus,
    `Advertising "Front Desk Printer" on _${serviceName}._tcp`,
    "ok",
  );

  try {
    // A fully generic advertisement: custom instance label, port, TXT and
    // protocol — this is not an iroh node.
    await node.advertise({
      serviceName,
      instanceName: "Front Desk Printer",
      port: 9100,
      protocol: "tcp",
      txt: { model: "LaserJet 9000", color: "true", pdl: "application/pdf" },
      signal: genericAdvertiseAbort.signal,
    });
  } catch { /* aborted or error */ }

  genericAdvertiseAbort = null;
  genericAdvertiseBtn.textContent = "Start advertising";
  setStatus(genericStatus, "Done");
});

genericBrowseBtn.addEventListener("click", async () => {
  if (genericBrowseAbort) {
    genericBrowseAbort.abort();
    genericBrowseAbort = null;
    genericBrowseBtn.textContent = "Start browsing";
    return;
  }

  const serviceName = genericServiceInput.value.trim() || "demo-printer";
  genericBrowseAbort = new AbortController();
  genericBrowseBtn.textContent = "Stop browsing";
  genericLog.textContent = "";

  try {
    for await (
      const record of node.browse({
        serviceName,
        protocol: "tcp",
        signal: genericBrowseAbort.signal,
      })
    ) {
      const icon = record.isActive ? "+" : "-";
      const txt = Object.entries(record.txt)
        .map(([k, v]) => `${k}=${v}`)
        .join(", ");
      // Generic DNS-SD instance labels are human service names, not implicit
      // iroh identities. Only classify/register records carrying a valid `pk`.
      let peer = null;
      const advertisedPk = record.txt.pk;
      if (advertisedPk) {
        try {
          PublicKey.fromString(advertisedPk);
          peer = asIrohPeer(record);
        } catch {
          /* ordinary non-iroh DNS-SD service */
        }
      }
      const tag = peer ? `  (iroh peer ${peer.nodeId.slice(0, 12)}…)` : "";
      if (peer && record.isActive) {
        upsertPeer({
          nodeId: peer.nodeId,
          source: "generic",
          platform: record.txt.platform,
          addrs: peer.addrs,
        });
      }
      // Greppable line for criterion 5 — on iOS a generic browse surfaces
      // metadata only, so expect `port=0 host=undefined`; on Android/desktop
      // these resolve to real values.
      dnssdCheck(null, {
        check: "generic",
        role: "browse",
        instance: record.instanceName,
        port: record.port,
        host: record.host ?? "undefined",
        addrs: record.addrs.length,
        isActive: record.isActive,
      });
      appendLog(
        genericLog,
        `${icon} ${record.instanceName} :${record.port}${tag}` +
          (txt ? `\n    ${txt}` : ""),
      );
    }
  } catch (e) {
    appendLog(genericLog, `Error: ${e}`);
  }

  genericBrowseAbort = null;
  genericBrowseBtn.textContent = "Start browsing";
});

// ── Sessions (QUIC) ────────────────────────────────────────────────────────────

const sessionPeerInput = document.querySelector<HTMLInputElement>(
  "#session-peer-input",
)!;
const sessionConnectBtn = document.querySelector<HTMLButtonElement>(
  "#session-connect-btn",
)!;
const sessionStatus = document.querySelector<HTMLElement>("#session-status")!;
const sessionLog = document.querySelector<HTMLElement>("#session-log")!;
const sessionControls = document.querySelector<HTMLElement>(
  "#session-controls",
)!;
const sessionMessageInput = document.querySelector<HTMLInputElement>(
  "#session-message-input",
)!;
const datagramInput = document.querySelector<HTMLInputElement>(
  "#datagram-input",
)!;

mountPeerPicker("session-peer-picker", (p) => {
  sessionPeerInput.value = p.nodeId;
});

let activeSession: IrohSession | null = null;
let bidiWriter: WritableStreamDefaultWriter<Uint8Array> | null = null;

sessionConnectBtn.addEventListener("click", async () => {
  const peer = sessionPeerInput.value.trim();
  if (!peer) {
    sessionPeerInput.focus();
    return;
  }
  if (/^[0-9a-z]{40,}$/i.test(peer)) {
    upsertPeer({ nodeId: peer, source: "session" });
  }

  setStatus(sessionStatus, "Connecting…");
  sessionConnectBtn.disabled = true;

  try {
    activeSession = await node.dial(peer);
    setStatus(sessionStatus, `Connected to ${peer.slice(0, 20)}…`, "ok");
    appendLog(sessionLog, "Session open");

    // Open a bidi stream for two-way messaging.
    const stream = await activeSession.createBidirectionalStream();
    bidiWriter = stream.writable.getWriter();

    // Read loop — receives echo replies (or messages from the remote peer).
    void (async () => {
      const reader = stream.readable.getReader();
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          appendLog(sessionLog, `← bidi: "${td.decode(value)}"`);
        }
      } catch { /* session closed */ }
      appendLog(sessionLog, "Bidi stream ended");
    })();

    // Datagram receive loop.
    void (async () => {
      const reader = activeSession!.datagrams.readable.getReader();
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) break;
          appendLog(sessionLog, `← datagram: "${td.decode(value)}"`);
        }
      } catch { /* session closed */ }
    })();

    sessionControls.classList.remove("hidden");
    sessionConnectBtn.textContent = "Connected";
  } catch (e) {
    setStatus(sessionStatus, String(e), "error");
    sessionConnectBtn.disabled = false;
  }
});

document.querySelector<HTMLButtonElement>("#session-send-btn")!
  .addEventListener("click", async () => {
    const msg = sessionMessageInput.value.trim();
    if (!msg || !bidiWriter) return;
    try {
      await bidiWriter.write(te.encode(msg));
      appendLog(sessionLog, `→ bidi: "${msg}"`);
      sessionMessageInput.value = "";
    } catch (e) {
      appendLog(sessionLog, `Send error: ${e}`);
    }
  });

document.querySelector<HTMLButtonElement>("#datagram-send-btn")!
  .addEventListener("click", async () => {
    const msg = datagramInput.value.trim();
    if (!msg || !activeSession) return;
    // Acquire and immediately release the lock for a one-shot write.
    const writer = activeSession.datagrams.writable.getWriter();
    try {
      await writer.write(te.encode(msg));
      appendLog(sessionLog, `→ datagram: "${msg}"`);
      datagramInput.value = "";
    } catch (e) {
      appendLog(sessionLog, `Datagram error: ${e}`);
    } finally {
      writer.releaseLock();
    }
  });

document.querySelector<HTMLButtonElement>("#session-close-btn")!
  .addEventListener("click", () => {
    if (!activeSession) return;
    activeSession.close({ closeCode: 0, reason: "user closed" });
    activeSession = null;
    bidiWriter = null;
    sessionControls.classList.add("hidden");
    sessionConnectBtn.textContent = "Connect";
    sessionConnectBtn.disabled = false;
    setStatus(sessionStatus, "Closed");
    appendLog(sessionLog, "Session closed by user");
  });

// ── Crypto ─────────────────────────────────────────────────────────────────────

const signDataInput = document.querySelector<HTMLInputElement>(
  "#sign-data-input",
)!;
const signatureOutput = document.querySelector<HTMLElement>(
  "#signature-output",
)!;
const verifyKeyInput = document.querySelector<HTMLInputElement>(
  "#verify-key-input",
)!;
const verifyDataInput = document.querySelector<HTMLInputElement>(
  "#verify-data-input",
)!;
const verifySigInput = document.querySelector<HTMLInputElement>(
  "#verify-sig-input",
)!;
const verifyResult = document.querySelector<HTMLElement>("#verify-result")!;
const genKeyOutput = document.querySelector<HTMLElement>("#gen-key-output")!;

document.querySelector<HTMLButtonElement>("#sign-btn")!.addEventListener(
  "click",
  async () => {
    const data = te.encode(signDataInput.value);
    try {
      const sig = await node.secretKey.sign(data);
      const hex = toHex(sig);
      signatureOutput.textContent = hex;
      // Pre-fill verify fields for a quick round-trip check.
      verifyKeyInput.value = node.publicKey.toString();
      verifyDataInput.value = signDataInput.value;
      verifySigInput.value = hex;
    } catch (e) {
      signatureOutput.textContent = String(e);
    }
  },
);

document.querySelector<HTMLButtonElement>("#verify-btn")!.addEventListener(
  "click",
  async () => {
    try {
      const pk = PublicKey.fromString(verifyKeyInput.value.trim());
      const data = te.encode(verifyDataInput.value);
      const sig = fromHex(verifySigInput.value);
      const valid = await pk.verify(data, sig);
      setStatus(
        verifyResult,
        valid ? "✓ Valid signature" : "✗ Invalid signature",
        valid ? "ok" : "error",
      );
    } catch (e) {
      setStatus(verifyResult, String(e), "error");
    }
  },
);

document.querySelector<HTMLButtonElement>("#gen-key-btn")!.addEventListener(
  "click",
  () => {
    const sk = SecretKey.generate();
    genKeyOutput.textContent = [
      `Secret key (hex):  ${toHex(sk.toBytes())}`,
      ``,
      `Usage: await createNode({ key: fromHex('<above hex>') })`,
    ].join("\n");
  },
);

// ── Test (cross-device interop harness, #340) ────────────────────────────────
//
// An opt-in, LAN-scoped "testing mode". While enabled this node BOTH serves the
// shared HTTP compliance handler AND advertises a `test=1` intent over generic
// DNS-SD so peer test-mode devices can discover it and run the suite against it.
// Pressing "Run" executes the suite one-way (this device = client) against a
// selected discovered peer and renders a per-case grid + structured JSON log.
//
// Safety: OFF by default, clearly indicated while on, and force-disabled on page
// teardown so it can never be left silently serving to the LAN.

const TEST_SERVICE = "iroh-http-test";
const TEST_TXT_KEY = "test";

type TestPlatform = "android" | "ios" | "desktop";

function detectPlatform(): TestPlatform {
  const ua = navigator.userAgent;
  if (/android/i.test(ua)) return "android";
  // iPadOS 13+ presents a Mac UA but reports touch points.
  if (/iphone|ipad|ipod/i.test(ua)) return "ios";
  if (/mac/i.test(ua) && navigator.maxTouchPoints > 1) return "ios";
  return "desktop";
}

// ── Test tab: interop suite runner ───────────────────────────────────────────
//
// Replaces the old per-case grid + disconnected DNS-SD check-cards with a single
// cohesive suite runner. Testing mode (opt-in, LAN-scoped) serves the shared
// compliance handler and advertises a `test=1` intent — now WITH the node's
// dialable `address` TXT (via `discoveryInfo()`) so peers can dial directly
// instead of over relay. The runner drives the structured `tests/interop`
// suite against a peer picked from the shared registry, grouped by suite group
// with status pills, latency, transport badges, a sticky summary, and a JSON
// export. The former #334 DNS-SD checks now live in the suite's `discovery`
// group. Greppable device tokens are preserved: `IROH_INTEROP_CASE` (per test,
// via the suite runner), `[iroh-http-interop]` (the JSON report), and
// `IROH_DNSSD_CHECK` (generic browse).
{
  const selfPlatform = detectPlatform();
  const selfId = node.publicKey.toString();

  const banner = document.querySelector<HTMLElement>("#test-mode-banner")!;
  const toggle = document.querySelector<HTMLInputElement>("#test-mode-toggle")!;
  const modeStatus = document.querySelector<HTMLElement>("#test-mode-status")!;
  const selfPlatformEl = document.querySelector<HTMLElement>(
    "#test-self-platform",
  )!;
  const selfIdEl = document.querySelector<HTMLElement>("#test-self-id")!;
  const testTabBtn = document.querySelector<HTMLButtonElement>(
    '[data-tab="test"]',
  )!;

  const runBtn = document.querySelector<HTMLButtonElement>("#suite-run-btn")!;
  const runAllBtn = document.querySelector<HTMLButtonElement>(
    "#suite-run-all-btn",
  )!;
  const autorunToggle = document.querySelector<HTMLInputElement>(
    "#suite-autorun-toggle",
  )!;
  const copyJsonBtn = document.querySelector<HTMLButtonElement>(
    "#suite-copy-json-btn",
  )!;
  const submitBtn = document.querySelector<HTMLButtonElement>(
    "#suite-submit-btn",
  )!;
  const autosubmitToggle = document.querySelector<HTMLInputElement>(
    "#suite-autosubmit-toggle",
  )!;
  const collectorIdInput = document.querySelector<HTMLInputElement>(
    "#suite-collector-id",
  )!;
  const collectorStatus = document.querySelector<HTMLElement>(
    "#suite-collector-status",
  )!;
  const targetLabel = document.querySelector<HTMLElement>("#suite-target")!;
  const summaryBar = document.querySelector<HTMLElement>("#suite-summary")!;
  const sumPass = document.querySelector<HTMLElement>("#suite-sum-pass")!;
  const sumFail = document.querySelector<HTMLElement>("#suite-sum-fail")!;
  const sumSkip = document.querySelector<HTMLElement>("#suite-sum-skip")!;
  const runStatus = document.querySelector<HTMLElement>("#suite-run-status")!;
  const sumMix = document.querySelector<HTMLElement>("#suite-sum-mix")!;
  const progressEl = document.querySelector<HTMLProgressElement>(
    "#suite-progress",
  )!;
  const groupsEl = document.querySelector<HTMLElement>("#suite-groups")!;
  const jsonLog = document.querySelector<HTMLElement>("#test-json-log")!;

  selfPlatformEl.textContent = selfPlatform;
  selfIdEl.textContent = `${selfId.slice(0, 16)}…`;
  selfIdEl.title = selfId;

  let testAbort: AbortController | null = null;
  // F10: retain the testing-mode serve handle so stop can await its shutdown
  // contract (ServeHandle.finished) instead of racing the next transition.
  let testServeHandle: { finished?: Promise<void> } | null = null;
  let running = false;
  let selectedPeerId: string | null = null;
  // Peers seen while testing mode is on — used for "Run all peers" and the
  // auto-run-on-new-peer trigger.
  const testPeers = new Map<string, RegistryPeer>();
  // F21: instance-name → nodeId, so an expiry event (empty TXT, so `asIrohPeer`
  // yields no pk) can still be mapped back to the peer it retires.
  const testInstanceToNodeId = new Map<string, string>();
  // A peer can be advertised under several DNS-SD instance names at once — iOS
  // renames its Bonjour instance on conflict (e.g. `pk` → `pk-<suffix>`), so the
  // same nodeId is briefly live under two names. Track the live instances per
  // nodeId so a stale instance's expiry only retires the peer once its *last*
  // instance is gone, instead of evicting a peer that is still advertised.
  const testNodeIdToInstances = new Map<string, Set<string>>();

  // F24: testing mode serves the compliance handler on the app's real,
  // relay-reachable node. The right exposure control here is *resource* bounds,
  // not an identity allow-list: every caller is already cryptographically
  // authenticated by QUIC (its public-key `Peer-Id`), and testing mode is an
  // explicit, ephemeral opt-in the operator toggles on. An earlier `testPeers`
  // allow-list 403-ed legitimately-authenticated peers whose generic test-mode
  // DNS-SD record failed to reach this node (iOS Bonjour instance renaming drops
  // the peer out of `testPeers`), which blocked the entire suite for an
  // iOS-as-client run. QUIC authentication already keeps the open internet from
  // reaching an unknown node id, so we drop the allow-list and keep only the
  // body-size + concurrency caps that stop an authenticated peer from
  // exhausting memory. Relay stays enabled so the relay-fallback group is
  // testable. `TEST_MAX_BODY_BYTES` is ≥ the largest fixture (body-size-5mb).
  const TEST_MAX_BODY_BYTES = 8 * 1024 * 1024;
  const TEST_MAX_INFLIGHT = 16;
  let testInflight = 0;

  async function handleTestingRequest(req: Request): Promise<Response> {
    // Reject oversized declared bodies before reading them.
    const declared = Number(req.headers.get("Content-Length") ?? "0");
    if (Number.isFinite(declared) && declared > TEST_MAX_BODY_BYTES) {
      return new Response("Payload too large\n", { status: 413 });
    }
    // Cap concurrent in-flight fixtures.
    if (testInflight >= TEST_MAX_INFLIGHT) {
      return new Response("Too many requests\n", { status: 429 });
    }
    testInflight += 1;
    try {
      return await handleComplianceRequest(req);
    } finally {
      testInflight -= 1;
    }
  }

  // ── Peer picker (top bar) ──────────────────────────────────────────────────
  mountPeerPicker(
    "test-peer-picker",
    (p) => {
      selectedPeerId = p.nodeId;
      refreshTarget();
      refreshRunEnabled();
    },
    { compact: true, placeholder: "Pick a peer to test against…" },
  );

  function refreshTarget(): void {
    if (!selectedPeerId) {
      targetLabel.textContent = "No peer selected — pick one above.";
      return;
    }
    const p = getPeers().find((x) => x.nodeId === selectedPeerId);
    targetLabel.textContent = p
      ? `Target: ${p.label} · ${p.platform} · ${
        p.addrs[0] ?? "no direct addr (relay only)"
      }`
      : `Target: ${selectedPeerId.slice(0, 16)}…`;
  }

  function refreshRunEnabled(): void {
    runBtn.disabled = running || selectedPeerId === null;
    // F21: batch runs target only active testing-mode peers, not the global
    // registry (which includes manual/session/generic peers).
    runAllBtn.disabled = running || testPeers.size === 0;
    refreshSubmitEnabled();
  }

  // ── Testing mode ───────────────────────────────────────────────────────────
  async function startTestingMode(): Promise<void> {
    if (testAbort) return;
    const ac = new AbortController();
    testAbort = ac;

    banner.classList.remove("hidden");
    testTabBtn.classList.add("testing-live");
    setStatus(modeStatus, "Serving compliance handler + advertising…", "ok");

    // (a) Serve the shared compliance handler so remote peers can run cases here.
    //     F10: if serve() rejects (e.g. the Server tab is already bound, or a
    //     previous test server is still draining), we must NOT advertise a
    //     compliance target we cannot actually serve. Abort, reset, and return.
    try {
      testServeHandle = node.serve(
        { signal: ac.signal },
        handleTestingRequest,
      );
    } catch (e) {
      ac.abort();
      testAbort = null;
      testServeHandle = null;
      banner.classList.add("hidden");
      testTabBtn.classList.remove("testing-live");
      toggle.checked = false;
      setStatus(
        modeStatus,
        `serve failed — testing mode not started: ${e}`,
        "error",
      );
      refreshRunEnabled();
      return;
    }

    // (b) Advertise our testing intent over generic DNS-SD alongside our pk.
    //     Attach our *dialable* direct address (and relay) via TXT so browsing
    //     peers resolve a real `ip:port` and can exercise a direct LAN dial
    //     instead of falling back to relay (the #350 "W1" gap). `discoveryInfo`
    //     surfaces the true bound QUIC port from core, which `node.addr()`
    //     cannot (its `ip_addrs()` ports are placeholders). The SRV `port: 1`
    //     stays a generic-record placeholder; the dialable address rides in the
    //     `address` TXT, which `asIrohPeer()` reads back on browse.
    const di = await node.discoveryInfo().catch(() => null);
    const dialable = di
      ? (di.directAddresses.length > 0
        ? di.directAddresses.join(",")
        : di.directAddress ?? undefined)
      : undefined;
    void node.advertise({
      serviceName: TEST_SERVICE,
      instanceName: selfId,
      port: 1,
      protocol: "udp",
      txt: {
        pk: selfId,
        [TEST_TXT_KEY]: "1",
        platform: selfPlatform,
        caps: "http-compliance",
        ...(dialable ? { [TXT_KEY_ADDRESS]: dialable } : {}),
        ...(di?.relayUrl ? { [TXT_KEY_RELAY]: di.relayUrl } : {}),
      },
      signal: ac.signal,
    }).catch(() => {/* aborted or error */});

    // (c) Browse for other test-mode peers → upsert into the shared registry.
    void (async () => {
      try {
        for await (
          const rec of node.browse({
            serviceName: TEST_SERVICE,
            protocol: "udp",
            signal: ac.signal,
          })
        ) {
          // F21: handle expiry/inactive events BEFORE the `test=1` TXT filter.
          // An expiry record carries empty TXT, so the marker check would skip
          // it and the peer would never be retired. Map it back by instance name.
          if (!rec.isActive) {
            const goneId = testInstanceToNodeId.get(rec.instanceName);
            if (goneId) {
              testInstanceToNodeId.delete(rec.instanceName);
              const live = testNodeIdToInstances.get(goneId);
              live?.delete(rec.instanceName);
              // Only retire the peer once its last live instance is gone: iOS
              // conflict-renames its Bonjour instance, so a stale instance's
              // expiry must not evict a peer still advertised under a new name.
              if (!live || live.size === 0) {
                testNodeIdToInstances.delete(goneId);
                testPeers.delete(goneId);
                if (selectedPeerId === goneId) refreshTarget();
                refreshRunEnabled();
              }
            }
            continue;
          }

          // Active records must carry the testing-mode marker to count.
          if (rec.txt[TEST_TXT_KEY] !== "1") continue;
          const peer = asIrohPeer(rec);
          if (!peer || peer.nodeId === selfId) continue;

          const isNew = !testPeers.has(peer.nodeId);
          const entry = upsertPeer({
            nodeId: peer.nodeId,
            source: "test",
            platform: rec.txt.platform,
            addrs: peer.addrs,
          });
          testPeers.set(peer.nodeId, entry);
          testInstanceToNodeId.set(rec.instanceName, peer.nodeId);
          let live = testNodeIdToInstances.get(peer.nodeId);
          if (!live) {
            live = new Set();
            testNodeIdToInstances.set(peer.nodeId, live);
          }
          live.add(rec.instanceName);
          refreshRunEnabled();
          if (isNew && autorunToggle.checked && !running) {
            selectedPeerId = peer.nodeId;
            refreshTarget();
            void runAgainstSelected();
          }
        }
      } catch { /* aborted */ }
    })();

    refreshRunEnabled();
  }

  async function stopTestingMode(): Promise<void> {
    if (!testAbort) return;
    testAbort.abort();
    testAbort = null;
    testPeers.clear();
    testInstanceToNodeId.clear();
    testNodeIdToInstances.clear();
    banner.classList.add("hidden");
    testTabBtn.classList.remove("testing-live");
    // F10: await the serve loop's shutdown contract (bounded) so a subsequent
    // start doesn't race a still-draining server on the same endpoint.
    const handle = testServeHandle;
    testServeHandle = null;
    if (handle?.finished) {
      try {
        await Promise.race([
          handle.finished,
          new Promise((r) => setTimeout(r, 5000)),
        ]);
      } catch {
        /* stop errors are non-fatal for the UI */
      }
    }
    setStatus(modeStatus, "Testing mode off.");
    refreshRunEnabled();
  }

  toggle.addEventListener("change", () => {
    if (toggle.checked) void startTestingMode();
    else void stopTestingMode();
  });

  // Force testing mode off on teardown — it must never outlive the page.
  const forceOff = () => {
    if (testAbort) {
      testAbort.abort();
      testAbort = null;
    }
  };
  globalThis.addEventListener("pagehide", forceOff);
  globalThis.addEventListener("beforeunload", forceOff);

  // ── Suite corpus ───────────────────────────────────────────────────────────
  // deno-lint-ignore no-explicit-any
  const allCases = (complianceCases as any[]).filter((c) =>
    c && c.id && !c.skip
  );
  const selfLoopbackCase = {
    id: "self-loopback",
    description: "self-request baseline (ADR-015)",
    request: { method: "GET", path: "/hello", headers: {}, body: null },
    response: { status: 200, bodyExact: "hello" },
  };

  // ── Result rendering ───────────────────────────────────────────────────────
  interface RowEls {
    row: HTMLDetailsElement;
    pill: HTMLElement;
    lat: HTMLElement;
    badge: HTMLElement;
    detail: HTMLElement;
  }
  const rowEls = new Map<string, RowEls>();
  const groupRolls = new Map<string, HTMLElement>();
  // deno-lint-ignore no-explicit-any
  let lastResults: any[] = [];

  // deno-lint-ignore no-explicit-any
  function renderSkeleton(groups: any[]): void {
    groupsEl.replaceChildren();
    rowEls.clear();
    groupRolls.clear();
    for (const group of groups) {
      const details = document.createElement("details");
      details.className = "suite-group";
      details.open = true;
      const summary = document.createElement("summary");
      const title = document.createElement("span");
      title.className = "suite-group-title";
      title.textContent = group.name;
      const roll = document.createElement("span");
      roll.className = "suite-group-roll";
      summary.append(title, roll);
      details.append(summary);
      groupRolls.set(group.id, roll);

      for (const test of group.tests) {
        const row = document.createElement("details");
        row.className = "suite-row";
        const rs = document.createElement("summary");
        const pill = document.createElement("span");
        pill.className = "pill pill-idle";
        pill.textContent = "•";
        const name = document.createElement("span");
        name.className = "suite-row-name";
        name.textContent = test.name;
        const lat = document.createElement("span");
        lat.className = "suite-row-lat";
        const badge = document.createElement("span");
        badge.className = "badge hidden";
        rs.append(pill, name, lat, badge);
        const detail = document.createElement("div");
        detail.className = "suite-row-detail";
        row.append(rs, detail);
        details.append(row);
        rowEls.set(test.id, { row, pill, lat, badge, detail });
      }
      groupsEl.append(details);
    }
  }

  // deno-lint-ignore no-explicit-any
  function updateRow(result: any): void {
    const els = rowEls.get(result.id);
    if (!els) return;
    const outcome = result.outcome as string;
    els.pill.className = `pill pill-${outcome}`;
    els.pill.textContent = outcome === "pass"
      ? "pass"
      : outcome === "fail"
      ? "fail"
      : outcome === "skip"
      ? "skip"
      : "run";
    els.lat.textContent = result.latencyMs != null
      ? `${result.latencyMs}ms`
      : "";
    if (result.transport) {
      els.badge.className = `badge badge-${result.transport}`;
      els.badge.textContent = result.transport;
      els.badge.classList.remove("hidden");
    }
    els.detail.textContent = result.detail ?? "";
    if (outcome === "fail") els.row.open = true;
  }

  function markRunning(id: string): void {
    const els = rowEls.get(id);
    if (!els) return;
    els.pill.className = "pill pill-running";
    els.pill.textContent = "…";
  }

  // deno-lint-ignore no-explicit-any
  function updateRollups(results: any[], groups: any[]): void {
    for (const group of groups) {
      const roll = groupRolls.get(group.id);
      if (!roll) continue;
      const ids = new Set(group.tests.map((t: { id: string }) => t.id));
      const rs = results.filter((r) => ids.has(r.id));
      const pass = rs.filter((r) => r.outcome === "pass").length;
      const fail = rs.filter((r) => r.outcome === "fail").length;
      const skip = rs.filter((r) => r.outcome === "skip").length;
      roll.textContent = `${pass}/${group.tests.length}` +
        (fail ? ` · ${fail} fail` : "") + (skip ? ` · ${skip} skip` : "");
      roll.className = "suite-group-roll" + (fail ? " has-fail" : "");
    }
  }

  // deno-lint-ignore no-explicit-any
  function updateSummary(summary: any): void {
    summaryBar.classList.remove("hidden");
    sumPass.textContent = `${summary.pass} pass`;
    sumFail.textContent = `${summary.fail} fail`;
    sumSkip.textContent = `${summary.skip} skip`;
    const t = summary.transport;
    sumMix.textContent =
      `transport: ${t.direct} direct · ${t.relay} relay · ${t.unknown} unknown`;
    const completed = summary.completed ?? summary.total;
    progressEl.max = Math.max(1, summary.total);
    progressEl.value = completed;
    runStatus.textContent = completed < summary.total
      ? `Running ${completed}/${summary.total}`
      : `Complete ${completed}/${summary.total}`;
    summaryBar.setAttribute(
      "aria-busy",
      completed < summary.total ? "true" : "false",
    );
  }

  function resetRunView(total: number): void {
    lastResults = [];
    lastPayload = null;
    jsonLog.textContent = "Suite running…";
    updateSummary({
      total,
      completed: 0,
      pass: 0,
      fail: 0,
      skip: 0,
      transport: { direct: 0, relay: 0, unknown: 0 },
    });
    refreshSubmitEnabled();
  }

  function markRunInterrupted(): void {
    runStatus.textContent = "Run interrupted";
    summaryBar.setAttribute("aria-busy", "false");
  }

  // ── Run ────────────────────────────────────────────────────────────────────
  // deno-lint-ignore no-explicit-any
  const reports: any[] = [];

  // ── Collector submit ───────────────────────────────────────────────────────
  // Post the run's JSON report to a standalone iroh-http collector node so
  // device results land on disk automatically instead of being copied by hand.
  // The collector id is remembered across launches. It may carry optional
  // address hints as `id|ip:port,https://relay` for direct and relay dialing.
  const COLLECTOR_STORAGE = "iroh-http-collector-id";
  // deno-lint-ignore no-explicit-any
  let lastPayload: any = null;

  // A build may bake in a default collector id (VITE_IROH_COLLECTOR_ID) so the
  // field is pre-filled and the operator can submit with one tap — no pasting a
  // 52-char node id on a phone. A user-entered value (localStorage) wins.
  const bakedCollector = (() => {
    try {
      // deno-lint-ignore no-explicit-any
      const env = (import.meta as any).env;
      const v = env?.VITE_IROH_COLLECTOR_ID;
      return typeof v === "string" ? v.trim() : "";
    } catch {
      return "";
    }
  })();
  const storedCollector = (() => {
    try {
      return localStorage.getItem(COLLECTOR_STORAGE) ?? "";
    } catch {
      return "";
    }
  })();
  const initialCollector = storedCollector || bakedCollector;
  if (initialCollector) collectorIdInput.value = initialCollector;

  function parseCollectorTarget(
    raw: string,
  ): { nodeId: string; addrs: string[] } | null {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    const [idPart, addrPart] = trimmed.split("|");
    const nodeId = idPart.trim();
    if (!nodeId) return null;
    const addrs = (addrPart ?? "")
      .split(",")
      .map((a) => a.trim())
      .filter((a) => a.length > 0);
    return { nodeId, addrs };
  }

  function refreshSubmitEnabled(): void {
    const hasTarget = parseCollectorTarget(collectorIdInput.value) !== null;
    submitBtn.disabled = running || !hasTarget || lastPayload === null;
  }

  async function submitToCollector(auto = false): Promise<void> {
    const target = parseCollectorTarget(collectorIdInput.value);
    if (!target || lastPayload === null) return;
    try {
      localStorage.setItem(COLLECTOR_STORAGE, collectorIdInput.value.trim());
    } catch {
      // Non-fatal: persistence is a convenience only.
    }
    setStatus(collectorStatus, auto ? "Auto-submitting…" : "Submitting…", "");
    try {
      const res = await node.fetch(`httpi://${target.nodeId}/results`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        ...dialHints(target.addrs),
        body: JSON.stringify(lastPayload),
      });
      if (res.ok) {
        // deno-lint-ignore no-explicit-any
        let saved: any = null;
        try {
          saved = await res.json();
        } catch {
          saved = null;
        }
        const n = Array.isArray(saved?.saved) ? saved.saved.length : "?";
        setStatus(collectorStatus, `Submitted ✓ (${n} saved)`, "ok");
      } else {
        setStatus(
          collectorStatus,
          `Collector responded ${res.status}`,
          "error",
        );
      }
    } catch (e) {
      setStatus(collectorStatus, `Submit failed: ${e}`, "error");
    }
  }

  collectorIdInput.addEventListener("input", () => {
    try {
      localStorage.setItem(COLLECTOR_STORAGE, collectorIdInput.value.trim());
    } catch {
      // ignore
    }
    refreshSubmitEnabled();
  });
  submitBtn.addEventListener("click", () => void submitToCollector(false));

  // deno-lint-ignore no-explicit-any
  function setLastPayload(payload: any): void {
    lastPayload = payload;
    refreshSubmitEnabled();
    if (autosubmitToggle.checked) void submitToCollector(true);
  }

  // deno-lint-ignore no-explicit-any
  async function runAgainst(peer: RegistryPeer | null): Promise<any> {
    const startedAt = new Date().toISOString();
    const groups = buildSuite({
      node,
      self: { nodeId: selfId, platform: selfPlatform },
      peer: peer
        ? { nodeId: peer.nodeId, platform: peer.platform, addrs: peer.addrs }
        : null,
      // deno-lint-ignore no-explicit-any
      fetch: (url: string, init: any) => node.fetch(url, init),
      cases: allCases,
      selfLoopbackCase,
      runCases: runComplianceCases,
      handler: handleComplianceRequest,
      createIsolatedNode: (request: { nodeOptions?: NodeOptions }) =>
        createNode(request.nodeOptions),
      helpers: { TXT_KEY_ADDRESS, TXT_KEY_RELAY, isDialableSocketAddr },
      isServing: testAbort !== null,
      // Tauri (mobile + desktop) has a working DNS-SD stack, so a discovery
      // round-trip that observes nothing is a real regression, not a skip.
      mdnsCapable: true,
    });

    renderSkeleton(groups);
    resetRunView(
      groups.reduce((sum: number, group: any) => sum + group.tests.length, 0),
    );

    const report = await runSuite(groups, {
      // deno-lint-ignore no-explicit-any
      onResult: (ev: any) => {
        if (ev.phase === "start") {
          markRunning(ev.test.id);
        } else if (ev.phase === "result") {
          updateRow(ev.result);
          lastResults = lastResults.filter((r) => r.id !== ev.result.id);
          lastResults.push(ev.result);
          updateRollups(lastResults, groups);
          updateSummary(ev.summary);
        }
      },
    });
    updateRollups(report.results, groups);
    updateSummary(report.summary);

    const full = {
      schema: "iroh-http-interop/2",
      startedAt,
      finishedAt: new Date().toISOString(),
      self: { publicKey: selfId, platform: selfPlatform },
      target: peer
        ? { publicKey: peer.nodeId, platform: peer.platform, addrs: peer.addrs }
        : null,
      summary: report.summary,
      results: report.results,
    };
    // Compact JSON to the console → reaches logcat/stdout on device.
    console.log("[iroh-http-interop]", JSON.stringify(full));
    return full;
  }

  async function runAgainstSelected(): Promise<void> {
    if (running) return;
    const peer = selectedPeerId
      ? getPeers().find((p) => p.nodeId === selectedPeerId) ?? null
      : null;
    running = true;
    lastResults = [];
    refreshRunEnabled();
    try {
      const report = await runAgainst(peer);
      reports.length = 0;
      reports.push(report);
      jsonLog.textContent = JSON.stringify(report, null, 2);
      setLastPayload(report);
    } catch (e) {
      markRunInterrupted();
      setStatus(modeStatus, `Run error: ${e}`, "error");
    } finally {
      running = false;
      refreshRunEnabled();
    }
  }

  runBtn.addEventListener("click", () => void runAgainstSelected());

  runAllBtn.addEventListener("click", async () => {
    if (running) return;
    // F21: only run against active testing-mode peers, not every registry peer.
    const peers = [...testPeers.values()];
    if (!peers.length) return;
    running = true;
    reports.length = 0;
    refreshRunEnabled();
    try {
      for (const peer of peers) {
        selectedPeerId = peer.nodeId;
        refreshTarget();
        lastResults = [];
        const report = await runAgainst(peer);
        reports.push(report);
      }
      jsonLog.textContent = JSON.stringify(
        { schema: "iroh-http-interop/2-batch", reports },
        null,
        2,
      );
      setLastPayload({
        schema: "iroh-http-interop/2-batch",
        reports: [...reports],
      });
    } catch (e) {
      markRunInterrupted();
      setStatus(modeStatus, `Run error: ${e}`, "error");
    } finally {
      running = false;
      refreshRunEnabled();
    }
  });

  copyJsonBtn.addEventListener("click", () => {
    const text = jsonLog.textContent ?? "";
    if (text) copyText(copyJsonBtn, () => text);
  });

  refreshTarget();
  refreshRunEnabled();
}
