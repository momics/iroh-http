/**
 * Reusable, searchable peer-picker.
 *
 * A single component embedded in the HTTP, Files, Peer, Sessions, and Test tabs.
 * It reads the shared {@link ./peer-registry} live, filters by a search box, and
 * calls `onSelect` with the chosen peer so each tab can fill its own target
 * field. Framework-free DOM so it drops into the existing dev console cleanly.
 */
import { getPeers, type RegistryPeer, subscribe } from "./peer-registry.js";

export interface PeerPicker {
  /** Root element to insert into a tab. */
  readonly element: HTMLElement;
  /** Stop listening to the registry and detach. */
  destroy(): void;
}

export interface PeerPickerOptions {
  /** Called when the operator picks a peer from the list. */
  onSelect: (peer: RegistryPeer) => void;
  /** Search-box placeholder. */
  placeholder?: string;
  /** Optional compact mode (used in the dense Test-tab top bar). */
  compact?: boolean;
}

function sourceBadge(source: RegistryPeer["source"]): HTMLElement {
  const span = document.createElement("span");
  span.className = `peer-src peer-src-${source}`;
  span.textContent = source;
  return span;
}

export function createPeerPicker(opts: PeerPickerOptions): PeerPicker {
  const root = document.createElement("div");
  root.className = "peer-picker" + (opts.compact ? " compact" : "");

  const search = document.createElement("input");
  search.type = "text";
  search.className = "peer-picker-search";
  search.placeholder = opts.placeholder ?? "Search discovered peers…";
  search.autocomplete = "off";
  search.spellcheck = false;

  const list = document.createElement("ul");
  list.className = "peer-picker-list";

  const empty = document.createElement("p");
  empty.className = "hint peer-picker-empty";
  empty.textContent = "No peers discovered yet.";

  root.append(search, empty, list);

  let current: RegistryPeer[] = getPeers();

  function render(): void {
    const q = search.value.trim().toLowerCase();
    const filtered = q
      ? current.filter(
        (p) =>
          p.nodeId.toLowerCase().includes(q) ||
          p.label.toLowerCase().includes(q) ||
          p.platform.toLowerCase().includes(q),
      )
      : current;

    list.replaceChildren();
    // F28: the empty-state must reflect the FILTERED result count, not the raw
    // peer count — otherwise a no-match search renders a blank panel with no
    // "nothing matches" hint. Adapt the message to distinguish "no peers yet"
    // from "no peers match the search".
    empty.textContent = current.length === 0
      ? "No peers discovered yet."
      : "No peers match your search.";
    empty.classList.toggle("hidden", filtered.length > 0);

    for (const peer of filtered) {
      const li = document.createElement("li");
      li.className = "peer-picker-item";
      li.tabIndex = 0;
      li.title = peer.nodeId;

      const main = document.createElement("div");
      main.className = "peer-picker-main";

      const id = document.createElement("code");
      id.className = "peer-id";
      id.textContent = peer.label;

      const meta = document.createElement("div");
      meta.className = "peer-picker-meta";
      const plat = document.createElement("span");
      plat.className = "peer-platform";
      plat.textContent = peer.platform || "?";
      meta.append(plat, sourceBadge(peer.source));
      if (peer.addrs.length) {
        const addr = document.createElement("span");
        addr.className = "peer-addr";
        addr.textContent = peer.addrs[0];
        meta.append(addr);
      }

      main.append(id, meta);
      li.append(main);

      const choose = () => opts.onSelect(peer);
      li.addEventListener("click", choose);
      li.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          choose();
        }
      });
      list.append(li);
    }
  }

  search.addEventListener("input", render);
  const unsubscribe = subscribe((peers) => {
    current = peers;
    render();
  });

  return {
    element: root,
    destroy() {
      unsubscribe();
      root.remove();
    },
  };
}
