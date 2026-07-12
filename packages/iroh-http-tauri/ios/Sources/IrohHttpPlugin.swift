import Foundation
import Network
import Tauri

// MARK: - Argument Types

struct BrowseStartArgs: Decodable {
    let serviceName: String
}

struct BrowsePollArgs: Decodable {
    let browseId: UInt64
}

struct BrowseStopArgs: Decodable {
    let browseId: UInt64
}

struct AdvertiseStartArgs: Decodable {
    let serviceName: String
    let pk: String
    let relay: String?
    /// Primary direct `ip:port` address to publish so browsing peers can dial
    /// this node over the LAN. Carries the real bound QUIC port (never 0). #346.
    let address: String?
}

struct AdvertiseStopArgs: Decodable {
    let advertiseId: UInt64
}

// Generic DNS-SD (arbitrary services, not iroh peers).

struct DnsSdAdvertiseStartArgs: Decodable {
    let serviceName: String
    let instanceName: String
    let port: UInt16
    let `protocol`: String
    let addrs: [String]
    let txt: [String: String]
}

struct DnsSdBrowseStartArgs: Decodable {
    let serviceName: String
    let `protocol`: String
}

// MARK: - Session Types

private final class BrowseSession {
    let id: UInt64
    let browser: NWBrowser
    var pendingEvents: [[String: Any]] = []
    // fullServiceName → (nodeId, signature). The signature is (nodeId + sorted
    // dialable addrs) so a peer that rebinds to a new address under the SAME
    // nodeId (the foreground-restart-rebind case, #336) re-emits instead of
    // being suppressed by a nodeId-only dedup (#350 review M2).
    var knownNodes: [String: (nodeId: String, signature: String)] = [:]

    init(id: UInt64, browser: NWBrowser) {
        self.id = id
        self.browser = browser
    }
}

private final class AdvertiseSession {
    let id: UInt64
    let listener: NWListener

    init(id: UInt64, listener: NWListener) {
        self.id = id
        self.listener = listener
    }
}

/// Snapshot of the resolved portion of a generic DNS-SD record, used to detect
/// when a known instance's TXT/addrs actually change so `browse_start` can
/// re-emit — instead of a one-shot "seen it once" dedup that would otherwise
/// never re-surface updates (unlike desktop, which re-announces on change).
private struct DnsSdRecordSnapshot: Equatable {
    let txt: [String: String]
    let addrs: [String]
}

/// A generic DNS-SD browse session. Unlike `BrowseSession` (which reduces every
/// result to a `(nodeId, addrs)` peer tuple), this keeps the full record shape.
private final class DnsSdBrowseSession {
    let id: UInt64
    let browser: NWBrowser
    let serviceType: String
    var pendingRecords: [[String: Any]] = []
    var knownInstances: [String: DnsSdRecordSnapshot] = [:]

    init(id: UInt64, browser: NWBrowser, serviceType: String) {
        self.id = id
        self.browser = browser
        self.serviceType = serviceType
    }
}

// MARK: - Plugin

@objc(IrohHttpPlugin)
class IrohHttpPlugin: Plugin {
    private let queue = DispatchQueue(label: "com.iroh.http.mdns")
    private var browseSessions: [UInt64: BrowseSession] = [:]
    private var advertiseSessions: [UInt64: AdvertiseSession] = [:]
    private var dnsSdBrowseSessions: [UInt64: DnsSdBrowseSession] = [:]
    private var nextBrowseId: UInt64 = 1
    private var nextAdvertiseId: UInt64 = 1

    // MARK: - Helpers

    /// Allocate a unique browse id.
    ///
    /// The increment runs on `queue` so concurrent browse/advertise commands
    /// (which arrive on arbitrary threads) can't tear it and hand out a
    /// duplicate id that would overwrite a live session (#350 review W4).
    private func allocBrowseId() -> UInt64 {
        queue.sync {
            defer { nextBrowseId += 1 }
            return nextBrowseId
        }
    }

    private func allocAdvertiseId() -> UInt64 {
        queue.sync {
            defer { nextAdvertiseId += 1 }
            return nextAdvertiseId
        }
    }

    /// Decode a flat DNS-SD TXT record byte blob into key-value pairs.
    private func parseTxtRecord(_ data: Data?) -> [String: String] {
        guard let data = data, !data.isEmpty else { return [:] }
        var result: [String: String] = [:]
        var idx = 0
        let bytes = [UInt8](data)
        while idx < bytes.count {
            let len = Int(bytes[idx])
            idx += 1
            guard len > 0, idx + len <= bytes.count else { break }
            let slice = bytes[idx ..< (idx + len)]
            idx += len
            if let eqIdx = slice.firstIndex(of: UInt8(ascii: "=")) {
                let key = String(bytes: slice[..<eqIdx], encoding: .utf8) ?? ""
                let val = String(bytes: slice[(eqIdx + 1)...], encoding: .utf8) ?? ""
                if !key.isEmpty { result[key] = val }
            }
        }
        return result
    }

    /// Encode key-value pairs into DNS-SD TXT record data.
    private func encodeTxtData(_ pairs: [String: String]) -> Data {
        var result = Data()
        for (key, value) in pairs {
            let entry = "\(key)=\(value)"
            guard let entryData = entry.data(using: .utf8), entryData.count <= 255 else { continue }
            result.append(UInt8(entryData.count))
            result.append(entryData)
        }
        return result
    }

    /// Validate that a string is a canonical iroh endpoint id: a 32-byte
    /// Ed25519 public key encoded as lowercase RFC 4648 base32 without padding,
    /// i.e. exactly 52 characters drawn from the `a-z` / `2-7` alphabet.
    ///
    /// Used to safely accept the DNS-SD instance name as the node-id when a
    /// peer's advertisement carries no `pk` TXT. Every current
    /// `advertise_peer` implementation (desktop's `mdns-sd`-backed advertiser
    /// included) sets `pk`, so this is a defensive fallback for
    /// advertisements from older or third-party peers rather than the normal
    /// path. The advertise side truncates instance names to 63 chars, which
    /// does not truncate a 52-char id, so the recovered id is always
    /// complete.
    private func isValidEndpointId(_ s: String) -> Bool {
        guard s.count == 52 else { return false }
        return s.allSatisfy { c in
            (c >= "a" && c <= "z") || (c >= "2" && c <= "7")
        }
    }

    // MARK: - Browse Commands

    @objc public func browse_peers_start(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowseStartArgs.self)
        let browseId = allocBrowseId()

        let serviceType = "_\(args.serviceName)._udp"
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: serviceType, domain: nil)
        let browser = NWBrowser(for: descriptor, using: .udp)
        let session = BrowseSession(id: browseId, browser: browser)

        browser.browseResultsChangedHandler = { [weak self] latestResults, _ in
            guard let self = self else { return }
            self.queue.async {
                self.handleBrowseResults(session: session, results: latestResults)
            }
        }

        browser.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                if case .dns(let code) = error, code == -65569 {
                    // Expected on duplicate/teardown; not worth logging.
                } else {
                    NSLog("[iroh-http-mdns] browse \(browseId) failed: \(error.localizedDescription)")
                }
                // #350 review L4: on any terminal failure cancel and drop the
                // session so it doesn't leak in the map.
                browser.cancel()
                self?.queue.async { self?.browseSessions.removeValue(forKey: browseId) }
            }
        }

        queue.async {
            self.browseSessions[browseId] = session
            browser.start(queue: self.queue)
        }

        invoke.resolve(["browseId": browseId])
    }

    private func handleBrowseResults(session: BrowseSession, results: Set<NWBrowser.Result>) {
        var currentPks: Set<String> = []

        for result in results {
            var txt: [String: String] = [:]
            if case let .bonjour(txtRecord) = result.metadata {
                if #available(iOS 16.0, *) {
                    txt = parseTxtRecord(txtRecord.data)
                } else {
                    for (key, entry) in txtRecord {
                        if case .string(let value) = entry { txt[key] = value }
                    }
                }
            }

            // The DNS-SD instance name doubles as the node-id fallback for
            // advertisements with no `pk` TXT. Desktop's `mdns-sd`-backed
            // advertiser publishes the base32 endpoint id as the instance
            // name *and* sets `pk`, so this only matters for older or
            // third-party advertisers.
            var instanceName: String? = nil
            if case .service(let name, _, _, _) = result.endpoint {
                instanceName = name
            }

            // Resolve the node-id: prefer the `pk` TXT (set by every current
            // advertiser), then fall back to the instance name. Reject
            // records where neither yields a valid id.
            let nodeId: String
            if let pk = txt["pk"], !pk.isEmpty {
                nodeId = pk
            } else if let name = instanceName, isValidEndpointId(name) {
                nodeId = name
            } else {
                continue
            }
            currentPks.insert(nodeId)

            var addrs: [String] = []
            // #346: a direct `ip:port` address published by the advertiser lets
            // this peer be dialed over the LAN. It already carries the real
            // bound QUIC port, so it is surfaced verbatim.
            if let address = txt["address"], !address.isEmpty { addrs.append(address) }
            if let relay = txt["relay"], !relay.isEmpty { addrs.append(relay) }

            if let name = instanceName {
                // #350 review M2: dedup on (nodeId + sorted addrs) so a rebind
                // under the same nodeId re-emits with the new address instead of
                // being suppressed.
                let signature = nodeId + "|" + addrs.sorted().joined(separator: ",")
                if session.knownNodes[name]?.signature != signature {
                    session.knownNodes[name] = (nodeId: nodeId, signature: signature)
                    session.pendingEvents.append([
                        "type": "discovered",
                        "nodeId": nodeId,
                        "addrs": addrs,
                    ])
                }
            }
        }

        // Emit "expired" for nodes that vanished from the result set.
        let expiredNames = session.knownNodes.filter { !currentPks.contains($0.value.nodeId) }.map { $0.key }
        for name in expiredNames {
            if let snapshot = session.knownNodes.removeValue(forKey: name) {
                session.pendingEvents.append(["type": "expired", "nodeId": snapshot.nodeId, "addrs": []])
            }
        }
    }

    @objc public func browse_peers_poll(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowsePollArgs.self)
        queue.async {
            guard let session = self.browseSessions[args.browseId] else {
                invoke.resolve(["events": [] as [[String: Any]]])
                return
            }
            let events = session.pendingEvents
            session.pendingEvents = []
            invoke.resolve(["events": events])
        }
    }

    @objc public func browse_peers_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowseStopArgs.self)
        queue.async {
            if let session = self.browseSessions.removeValue(forKey: args.browseId) {
                session.browser.cancel()
            }
        }
        invoke.resolve()
    }

    // MARK: - Advertise Commands

    @objc public func advertise_peer_start(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(AdvertiseStartArgs.self)
        let advertiseId = allocAdvertiseId()

        let serviceType = "_\(args.serviceName)._udp"
        let listener: NWListener
        do {
            listener = try NWListener(using: .udp)
        } catch {
            invoke.reject("Failed to create listener: \(error.localizedDescription)")
            return
        }

        var txtPairs: [String: String] = ["pk": args.pk]
        if let relay = args.relay { txtPairs["relay"] = relay }
        // #346: publish the node's direct `ip:port` address so peers can dial it
        // over the LAN. The iOS `NWListener`'s own UDP port is unrelated to the
        // QUIC socket, so the SRV port is not usable — the reconciled address
        // (with the real bound QUIC port) travels in this TXT entry instead.
        if let address = args.address, !address.isEmpty { txtPairs["address"] = address }
        let txtData = encodeTxtData(txtPairs)

        listener.service = NWListener.Service(
            name: String(args.pk.prefix(63)),
            type: serviceType,
            domain: nil,
            txtRecord: txtData
        )
        listener.newConnectionHandler = { conn in conn.cancel() }

        listener.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                if case .dns(let code) = error, code == -65569 {
                    // Expected on duplicate/teardown; not worth logging.
                } else {
                    NSLog("[iroh-http-mdns] advertise \(advertiseId) failed: \(error.localizedDescription)")
                }
                // #350 review L4: on any terminal failure cancel and drop the
                // session so it doesn't leak in the map.
                listener.cancel()
                self?.queue.async { self?.advertiseSessions.removeValue(forKey: advertiseId) }
            }
        }

        queue.async {
            self.advertiseSessions[advertiseId] = AdvertiseSession(id: advertiseId, listener: listener)
            listener.start(queue: self.queue)
        }

        invoke.resolve(["advertiseId": advertiseId])
    }

    @objc public func advertise_peer_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(AdvertiseStopArgs.self)
        queue.async {
            if let session = self.advertiseSessions.removeValue(forKey: args.advertiseId) {
                session.listener.cancel()
            }
        }
        invoke.resolve()
    }

    // MARK: - Generic DNS-SD Commands

    /// Map a DNS-SD protocol string to a `NWParameters` transport. Defaults to
    /// UDP for any unrecognised value.
    private func parameters(for proto: String) -> NWParameters {
        proto.lowercased() == "tcp" ? .tcp : .udp
    }

    @objc public func browse_start(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(DnsSdBrowseStartArgs.self)
        let browseId = allocBrowseId()

        let proto = args.`protocol`.lowercased() == "tcp" ? "_tcp" : "_udp"
        let serviceType = "_\(args.serviceName).\(proto)"
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: serviceType, domain: nil)
        let browser = NWBrowser(for: descriptor, using: parameters(for: args.`protocol`))
        let session = DnsSdBrowseSession(id: browseId, browser: browser, serviceType: serviceType)

        browser.browseResultsChangedHandler = { [weak self] latestResults, _ in
            guard let self = self else { return }
            self.queue.async {
                self.handleDnsSdBrowseResults(session: session, results: latestResults)
            }
        }

        browser.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                if case .dns(let code) = error, code == -65569 {
                    // Expected on duplicate/teardown; not worth logging.
                } else {
                    NSLog("[iroh-http-dnssd] browse \(browseId) failed: \(error.localizedDescription)")
                }
                // #350 review L4: on any terminal failure cancel and drop the
                // session so it doesn't leak in the map.
                browser.cancel()
                self?.queue.async { self?.dnsSdBrowseSessions.removeValue(forKey: browseId) }
            }
        }

        queue.async {
            self.dnsSdBrowseSessions[browseId] = session
            browser.start(queue: self.queue)
        }

        invoke.resolve(["browseId": browseId])
    }

    /// Build generic DNS-SD records from browse results.
    ///
    /// Note: `NWBrowser` surfaces the instance name, service type and TXT record
    /// but does *not* resolve the target host, port or IP addresses — that would
    /// require opening an `NWConnection` per result. iOS records are therefore
    /// metadata-only (`host = nil`, `port = 0`, `addrs = []`); a `relay`/`address`
    /// TXT entry, if present, is surfaced through `addrs` as a best effort.
    private func handleDnsSdBrowseResults(
        session: DnsSdBrowseSession, results: Set<NWBrowser.Result>
    ) {
        var current: Set<String> = []

        for result in results {
            guard case .service(let name, let type, _, _) = result.endpoint else { continue }
            current.insert(name)

            var txt: [String: String] = [:]
            if case let .bonjour(txtRecord) = result.metadata {
                if #available(iOS 16.0, *) {
                    txt = parseTxtRecord(txtRecord.data)
                } else {
                    for (key, entry) in txtRecord {
                        if case .string(let value) = entry { txt[key] = value }
                    }
                }
            }

            var addrs: [String] = []
            if let addr = txt["address"], !addr.isEmpty { addrs.append(addr) }
            if let relay = txt["relay"], !relay.isEmpty { addrs.append(relay) }

            let snapshot = DnsSdRecordSnapshot(txt: txt, addrs: addrs)
            if session.knownInstances[name] == snapshot { continue }
            let isReemit = session.knownInstances[name] != nil
            session.knownInstances[name] = snapshot
            // #334: greppable trace of the re-emit dedup. A known instance whose
            // TXT/addrs changed re-surfaces here (`event=reemit`) instead of
            // being suppressed forever by a one-shot Set. `rev` mirrors the
            // example app's mutate counter when present.
            NSLog(
                "IROH_DNSSD_CHECK reemit instance=\(name) event=\(isReemit ? "reemit" : "new") rev=\(txt["rev"] ?? "-")"
            )

            session.pendingRecords.append([
                "isActive": true,
                "serviceType": type,
                "instanceName": name,
                "host": NSNull(),
                "port": 0,
                "addrs": addrs,
                "txt": txt,
            ])
        }

        // Emit inactive records for instances that vanished.
        let expired = Set(session.knownInstances.keys).subtracting(current)
        for name in expired {
            session.knownInstances.removeValue(forKey: name)
            session.pendingRecords.append([
                "isActive": false,
                "serviceType": session.serviceType,
                "instanceName": name,
                "host": NSNull(),
                "port": 0,
                "addrs": [String](),
                "txt": [String: String](),
            ])
        }
    }

    @objc public func browse_poll(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowsePollArgs.self)
        queue.async {
            guard let session = self.dnsSdBrowseSessions[args.browseId] else {
                invoke.resolve(["records": [] as [[String: Any]]])
                return
            }
            let records = session.pendingRecords
            session.pendingRecords = []
            invoke.resolve(["records": records])
        }
    }

    @objc public func browse_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowseStopArgs.self)
        queue.async {
            if let session = self.dnsSdBrowseSessions.removeValue(forKey: args.browseId) {
                session.browser.cancel()
            }
        }
        invoke.resolve()
    }

    @objc public func advertise_start(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(DnsSdAdvertiseStartArgs.self)
        let advertiseId = allocAdvertiseId()

        let proto = args.`protocol`.lowercased() == "tcp" ? "_tcp" : "_udp"
        let serviceType = "_\(args.serviceName).\(proto)"
        let params = parameters(for: args.`protocol`)

        let listener: NWListener
        do {
            if let port = NWEndpoint.Port(rawValue: args.port), args.port != 0 {
                listener = try NWListener(using: params, on: port)
            } else {
                listener = try NWListener(using: params)
            }
        } catch {
            invoke.reject("Failed to create listener: \(error.localizedDescription)")
            return
        }

        let txtData = encodeTxtData(args.txt)
        listener.service = NWListener.Service(
            name: String(args.instanceName.prefix(63)),
            type: serviceType,
            domain: nil,
            txtRecord: txtData
        )
        listener.newConnectionHandler = { conn in conn.cancel() }

        listener.stateUpdateHandler = { [weak self] state in
            if case .failed(let error) = state {
                if case .dns(let code) = error, code == -65569 {
                    // Expected on duplicate/teardown; not worth logging.
                } else {
                    NSLog("[iroh-http-dnssd] advertise \(advertiseId) failed: \(error.localizedDescription)")
                }
                // #350 review L4: on any terminal failure cancel and drop the
                // session so it doesn't leak in the map.
                listener.cancel()
                self?.queue.async { self?.advertiseSessions.removeValue(forKey: advertiseId) }
            }
        }

        queue.async {
            self.advertiseSessions[advertiseId] = AdvertiseSession(id: advertiseId, listener: listener)
            listener.start(queue: self.queue)
        }

        invoke.resolve(["advertiseId": advertiseId])
    }

    @objc public func advertise_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(AdvertiseStopArgs.self)
        queue.async {
            if let session = self.advertiseSessions.removeValue(forKey: args.advertiseId) {
                session.listener.cancel()
            }
        }
        invoke.resolve()
    }

    /// Query the platform's active-network DNS nameservers.
    ///
    /// This exists to feed iroh's resolver on Android, where iroh cannot read
    /// the system DNS config (no `/etc/resolv.conf`; servers live in
    /// `LinkProperties`). iOS has no such gap — iroh's default resolver reads
    /// the system configuration fine — and the public SDK deliberately does not
    /// expose the active resolvers. So this returns an empty list, which the
    /// Rust side (`commands.rs`) treats as "fall back to iroh's default
    /// resolver". Present so the Swift↔Kotlin FFI command surface stays in
    /// parity (see the `ffi_contract` test).
    @objc public func get_dns_servers(_ invoke: Invoke) throws {
        invoke.resolve(["servers": [] as [String]])
    }
}

// MARK: - Entry point

@_cdecl("init_plugin_iroh_http")
func initPlugin() -> Plugin {
    return IrohHttpPlugin()
}
