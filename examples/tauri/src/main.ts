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
  PublicKey,
  SecretKey,
} from "@momics/iroh-http-tauri";
import type { IrohSession } from "@momics/iroh-http-shared";
// @ts-ignore — .mjs from the shared cross-runtime compliance suite (pure WHATWG).
import { handleRequest as handleComplianceRequest } from "../../../tests/http-compliance/handler.mjs";
// @ts-ignore — .mjs from the shared compliance suite (pure WHATWG).
import { runCases as runComplianceCases } from "../../../tests/http-compliance/harness.mjs";
import complianceCases from "../../../tests/http-compliance/cases.json";

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

  try {
    const res = await node.fetch(`httpi://${peer}${path}`, {
      method: methodSelect.value,
    });
    setStatus(responseStatus, `HTTP ${res.status}`, res.ok ? "ok" : "error");
    responseBody.textContent = await res.text();
  } catch (e) {
    setStatus(responseStatus, "error", "error");
    responseBody.textContent = String(e);
  }
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
  return `httpi://${host}${p.startsWith("/") ? p : `/${p}`}`;
}

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
      const peer = asIrohPeer(record);
      const tag = peer ? `  (iroh peer ${peer.nodeId.slice(0, 12)}…)` : "";
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

let activeSession: IrohSession | null = null;
let bidiWriter: WritableStreamDefaultWriter<Uint8Array> | null = null;

sessionConnectBtn.addEventListener("click", async () => {
  const peer = sessionPeerInput.value.trim();
  if (!peer) {
    sessionPeerInput.focus();
    return;
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

interface TestPeer {
  nodeId: string;
  platform: string;
  addrs: string[];
}

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
  const peersList = document.querySelector<HTMLUListElement>(
    "#test-peers-list",
  )!;
  const peersEmpty = document.querySelector<HTMLElement>("#test-peers-empty")!;
  const runBtn = document.querySelector<HTMLButtonElement>("#test-run-btn")!;
  const runSummary = document.querySelector<HTMLElement>("#test-run-summary")!;
  const gridBody = document.querySelector<HTMLElement>("#test-grid-body")!;
  const jsonLog = document.querySelector<HTMLElement>("#test-json-log")!;
  const copyJsonBtn = document.querySelector<HTMLButtonElement>(
    "#test-copy-json-btn",
  )!;
  const testTabBtn = document.querySelector<HTMLButtonElement>(
    '[data-tab="test"]',
  )!;

  selfPlatformEl.textContent = selfPlatform;
  selfIdEl.textContent = `${selfId.slice(0, 16)}…`;
  selfIdEl.title = selfId;

  // Discovered test peers, keyed by node id.
  const testPeers = new Map<string, TestPeer>();
  let selectedPeerId: string | null = null;
  let testAbort: AbortController | null = null;
  let running = false;

  function refreshRunEnabled(): void {
    runBtn.disabled = !testAbort || running || selectedPeerId === null ||
      !testPeers.has(selectedPeerId);
  }

  function renderPeers(): void {
    peersList.replaceChildren();
    for (const peer of testPeers.values()) {
      const li = document.createElement("li");
      li.className = "peer-item selectable";
      if (peer.nodeId === selectedPeerId) li.classList.add("selected");

      const code = document.createElement("code");
      code.className = "peer-id";
      code.textContent = `${peer.nodeId.slice(0, 16)}…`;
      code.title = peer.nodeId;

      const plat = document.createElement("span");
      plat.className = "peer-platform";
      plat.textContent = peer.platform || "?";

      li.append(code, plat);
      li.addEventListener("click", () => {
        selectedPeerId = peer.nodeId;
        renderPeers();
        refreshRunEnabled();
      });
      peersList.append(li);
    }
    peersEmpty.classList.toggle("hidden", testPeers.size > 0);
  }

  async function startTestingMode(): Promise<void> {
    if (testAbort) return;
    const ac = new AbortController();
    testAbort = ac;

    banner.classList.remove("hidden");
    testTabBtn.classList.add("testing-live");
    setStatus(modeStatus, "Serving compliance handler + advertising…", "ok");

    // (a) Serve the shared compliance handler so remote peers can run cases here.
    try {
      node.serve({ signal: ac.signal }, handleComplianceRequest);
    } catch (e) {
      setStatus(modeStatus, `serve failed: ${e}`, "error");
    }

    // (b) Advertise our testing intent over generic DNS-SD alongside our pk.
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
      },
      signal: ac.signal,
    }).catch(() => {/* aborted or error */});

    // (c) Browse for other test-mode peers.
    void (async () => {
      try {
        for await (
          const rec of node.browse({
            serviceName: TEST_SERVICE,
            protocol: "udp",
            signal: ac.signal,
          })
        ) {
          if (rec.txt[TEST_TXT_KEY] !== "1") continue;
          const peer = asIrohPeer(rec);
          if (!peer || peer.nodeId === selfId) continue;

          if (rec.isActive) {
            testPeers.set(peer.nodeId, {
              nodeId: peer.nodeId,
              platform: rec.txt.platform ?? "?",
              addrs: peer.addrs,
            });
          } else {
            testPeers.delete(peer.nodeId);
            if (selectedPeerId === peer.nodeId) selectedPeerId = null;
          }
          renderPeers();
          refreshRunEnabled();
        }
      } catch {/* aborted */}
    })();

    refreshRunEnabled();
  }

  function stopTestingMode(): void {
    if (!testAbort) return;
    testAbort.abort();
    testAbort = null;
    testPeers.clear();
    selectedPeerId = null;
    renderPeers();
    banner.classList.add("hidden");
    testTabBtn.classList.remove("testing-live");
    setStatus(modeStatus, "Testing mode off.");
    refreshRunEnabled();
  }

  toggle.addEventListener("change", () => {
    if (toggle.checked) void startTestingMode();
    else stopTestingMode();
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

  // ── Run + rendering ────────────────────────────────────────────────────────

  interface CaseRow {
    id: string;
    ok: boolean;
    status: number | null;
    latencyMs: number;
    error: string | null;
  }

  function appendGridRow(index: number, r: CaseRow): void {
    const tr = document.createElement("tr");
    tr.className = r.ok ? "row-pass" : "row-fail";
    const cells = [
      String(index),
      r.id,
      r.ok ? "✓ pass" : "✗ fail",
      `${r.latencyMs}ms`,
      r.status === null ? "—" : String(r.status),
      r.error ?? "",
    ];
    cells.forEach((text, col) => {
      const td = document.createElement("td");
      td.textContent = text;
      if (col === 2) td.className = "result";
      if (col === 5) td.className = "err";
      tr.append(td);
    });
    gridBody.append(tr);
  }

  // The compliance corpus, minus JSON comment entries.
  // deno-lint-ignore no-explicit-any
  const allCases = (complianceCases as any[]).filter((c) => c && c.id);

  // Case 0: self/loopback baseline (ADR-015) to isolate transport vs platform.
  const selfLoopbackCase = {
    id: "self-loopback",
    description: "self-request baseline (ADR-015)",
    request: { method: "GET", path: "/hello", headers: {}, body: null },
    response: { status: 200, bodyExact: "hello" },
  };

  runBtn.addEventListener("click", async () => {
    if (!testAbort || running || selectedPeerId === null) return;
    const target = testPeers.get(selectedPeerId);
    if (!target) return;

    running = true;
    refreshRunEnabled();
    gridBody.replaceChildren();
    jsonLog.textContent = "";
    setStatus(runSummary, "Running…");

    const startedAt = new Date().toISOString();
    // Correlates the per-case and summary log lines for one run.
    const runId = (globalThis.crypto?.randomUUID?.() ??
      `${Date.now()}-${Math.random().toString(36).slice(2)}`);
    const rows: CaseRow[] = [];
    let index = 0;

    const record = (r: CaseRow) => {
      const seq = index++;
      appendGridRow(seq, r);
      rows.push(r);
      // Grep-anchored single line per case for adb logcat / iOS device logs.
      console.log(
        "IROH_INTEROP_CASE " +
          JSON.stringify({
            runId,
            seq,
            self: selfPlatform,
            target: target.platform,
            id: r.id,
            ok: r.ok,
            status: r.status,
            latencyMs: r.latencyMs,
            error: r.error,
          }),
      );
    };

    try {
      // Case 0 — loopback against ourselves (we are already serving).
      await runComplianceCases({
        // deno-lint-ignore no-explicit-any
        fetch: (url: string, init: any) => node.fetch(url, init),
        serverId: selfId,
        cases: [selfLoopbackCase],
        onCaseResult: record,
      });

      // Cases 1..N — the full suite against the selected peer (one-way).
      await runComplianceCases({
        // deno-lint-ignore no-explicit-any
        fetch: (url: string, init: any) => node.fetch(url, init),
        serverId: target.nodeId,
        directAddrs: target.addrs.length ? target.addrs : undefined,
        cases: allCases,
        onCaseResult: record,
      });
    } catch (e) {
      setStatus(runSummary, `Run error: ${e}`, "error");
    }

    const passed = rows.filter((r) => r.ok).length;
    const failed = rows.length - passed;
    setStatus(
      runSummary,
      failed > 0
        ? `✗ ${failed} failed, ${passed} passed`
        : `✓ all ${passed} passed`,
      failed > 0 ? "error" : "ok",
    );

    const report = {
      schema: "iroh-http-interop/1",
      runId,
      startedAt,
      finishedAt: new Date().toISOString(),
      direction: "outbound",
      self: { publicKey: selfId, platform: selfPlatform },
      target: { publicKey: target.nodeId, platform: target.platform },
      summary: { total: rows.length, passed, failed },
      cases: rows,
    };
    // On-screen: pretty, human-readable.
    jsonLog.textContent = JSON.stringify(report, null, 2);
    // Console: a single grep-anchored line for the whole run, so the operator
    // can pull it verbatim from `adb logcat` / iOS device logs and parse it.
    console.log("IROH_INTEROP_LOG " + JSON.stringify(report));

    running = false;
    refreshRunEnabled();
  });

  copyJsonBtn.addEventListener("click", () => {
    const text = jsonLog.textContent ?? "";
    if (text) copyText(copyJsonBtn, () => text);
  });

  renderPeers();
  refreshRunEnabled();
}
