import Darwin
import Foundation
import Network
import Tauri

// MARK: - Argument Types

struct BrowsePollArgs: Decodable {
    let browseId: UInt64
}

struct BrowseStopArgs: Decodable {
    let browseId: UInt64
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

struct DnsSdAdvertiseUpdateArgs: Decodable {
    let advertiseId: UInt64
    let port: UInt16
    let addrs: [String]
    let txt: [String: String]
}

struct DnsSdBrowseStartArgs: Decodable {
    let serviceName: String
    let `protocol`: String
}

// MARK: - Session Types

/// Tauri invokes may be completed by asynchronous native callbacks. Keep a
/// small one-shot guard at that boundary so readiness/failure/cancellation
/// races can never resolve and reject the same command.
private final class InvokeOnce {
    private let invoke: Invoke
    private let lock = NSLock()
    private var completed = false

    init(_ invoke: Invoke) {
        self.invoke = invoke
    }

    func resolve(_ payload: [String: Any]) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        lock.unlock()
        invoke.resolve(payload)
    }

    func resolve() {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        lock.unlock()
        invoke.resolve()
    }

    func reject(_ message: String) {
        lock.lock()
        guard !completed else {
            lock.unlock()
            return
        }
        completed = true
        lock.unlock()
        invoke.reject(message)
    }
}

private final class NetServiceRegistrationDelegate: NSObject, NetServiceDelegate {
    private let onPublished: () -> Void
    private let onFailure: (String) -> Void
    private let onStopped: () -> Void

    init(
        onPublished: @escaping () -> Void,
        onFailure: @escaping (String) -> Void,
        onStopped: @escaping () -> Void
    ) {
        self.onPublished = onPublished
        self.onFailure = onFailure
        self.onStopped = onStopped
    }

    func netServiceDidPublish(_ sender: NetService) {
        onPublished()
    }

    func netService(_ sender: NetService, didNotPublish errorDict: [String: NSNumber]) {
        let detail = errorDict
            .map { "\($0.key)=\($0.value)" }
            .sorted()
            .joined(separator: ", ")
        onFailure(detail.isEmpty ? "unknown NetService publish failure" : detail)
    }

    func netServiceDidStop(_ sender: NetService) {
        onStopped()
    }
}

private final class AdvertiseSession {
    let id: UInt64
    let service: NetService
    let registrationDelegate: NetServiceRegistrationDelegate
    let port: UInt16
    let startInvoke: InvokeOnce
    let lifecycle: DiscoveryAdvertisementLifecycle
    var pendingStopCompletions: [InvokeOnce] = []

    init(
        id: UInt64,
        service: NetService,
        registrationDelegate: NetServiceRegistrationDelegate,
        port: UInt16,
        startInvoke: InvokeOnce
    ) {
        self.id = id
        self.service = service
        self.registrationDelegate = registrationDelegate
        self.port = port
        self.startInvoke = startInvoke
        self.lifecycle = DiscoveryAdvertisementLifecycle(id: id, callbackGeneration: id)
    }
}

/// A generic DNS-SD browse session retaining the full record shape.
private final class DnsSdBrowseSession {
    let id: UInt64
    let browser: NWBrowser
    let serviceType: String
    let startInvoke: InvokeOnce
    let lifecycle: DiscoveryBrowseLifecycle
    var pendingStopCompletions: [InvokeOnce] = []

    init(id: UInt64, browser: NWBrowser, serviceType: String, startInvoke: InvokeOnce) {
        self.id = id
        self.browser = browser
        self.serviceType = serviceType
        self.startInvoke = startInvoke
        self.lifecycle = DiscoveryBrowseLifecycle(id: id, callbackGeneration: id)
    }
}

// MARK: - Plugin

@objc(IrohHttpPlugin)
class IrohHttpPlugin: Plugin {
    private let queue = DispatchQueue(label: "com.iroh.http.mdns")
    private var advertiseSessions: [UInt64: AdvertiseSession] = [:]
    private var dnsSdBrowseSessions: [UInt64: DnsSdBrowseSession] = [:]
    private var nextBrowseId: UInt64 = 1
    private var nextAdvertiseId: UInt64 = 1

    private enum TxtEncodingError: LocalizedError {
        case invalidKey(String)
        case oversizedEntry(String)

        var errorDescription: String? {
            switch self {
            case .invalidKey(let key):
                return "Invalid DNS-SD TXT key: \(key)"
            case .oversizedEntry(let key):
                return "DNS-SD TXT entry exceeds 255 bytes: \(key)"
            }
        }
    }

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
            } else {
                let key = String(bytes: slice, encoding: .utf8) ?? ""
                if !key.isEmpty { result[key] = "" }
            }
        }
        return result
    }

    /// Decode the entry-based NWTXTRecord representation available before
    /// iOS 16. `.empty` is a present key with no value, not an absent key; it
    /// must survive snapshots so add/remove transitions can be observed.
    /// Internal visibility keeps this legacy branch host-contract-testable.
    func decodeLegacyTxtRecord(_ record: NWTXTRecord) -> [String: String] {
        var result: [String: String] = [:]
        for (key, entry) in record {
            switch entry {
            case .string(let value):
                result[key] = value
            case .empty:
                result[key] = ""
            case .none:
                continue
            case .data:
                // This helper is only selected before iOS 16, where `.data`
                // cannot occur. The case keeps newer SDK enums exhaustive.
                continue
            @unknown default:
                continue
            }
        }
        return result
    }

    /// One TXT decoder shared by peer and generic browse paths.
    private func decodeTxtRecord(_ record: NWTXTRecord) -> [String: String] {
        if #available(iOS 16.0, *) {
            return parseTxtRecord(record.data)
        }
        return decodeLegacyTxtRecord(record)
    }

    /// Encode key-value pairs into DNS-SD TXT record data without silently
    /// dropping fields. DNS-SD limits each length-prefixed TXT entry to 255
    /// bytes, so callers get a start/update rejection when fidelity cannot be
    /// preserved.
    private func encodeTxtData(_ pairs: [String: String]) throws -> Data {
        var result = Data()
        for key in pairs.keys.sorted() {
            guard !key.isEmpty, !key.contains("=") else {
                throw TxtEncodingError.invalidKey(key)
            }
            let value = pairs[key] ?? ""
            let entry = "\(key)=\(value)"
            guard let entryData = entry.data(using: .utf8), entryData.count <= 255 else {
                throw TxtEncodingError.oversizedEntry(key)
            }
            result.append(UInt8(entryData.count))
            result.append(entryData)
        }
        return result
    }

    private func advertisementDidPublish(_ advertiseId: UInt64) {
        guard let session = advertiseSessions[advertiseId] else { return }
        if session.lifecycle.nativePublished(generation: advertiseId) {
            session.startInvoke.resolve(["advertiseId": advertiseId])
        }
    }

    /// Ask NetService to stop on the run loop that owns it. The session and
    /// delegate remain retained until `netServiceDidStop` proves native
    /// ownership is terminal.
    private func requestAdvertisementStop(_ session: AdvertiseSession) {
        DispatchQueue.main.async {
            dispatchPrecondition(condition: .onQueue(DispatchQueue.main))
            session.service.stop()
        }
    }

    /// Release the run-loop schedule and delegate only after didStop.
    private func finalizeAdvertisementService(_ session: AdvertiseSession) {
        DispatchQueue.main.async {
            dispatchPrecondition(condition: .onQueue(DispatchQueue.main))
            session.service.remove(from: RunLoop.main, forMode: .default)
            session.service.delegate = nil
        }
    }

    private func advertisementDidFail(_ advertiseId: UInt64, message: String) {
        guard
            let session = advertiseSessions[advertiseId],
            session.lifecycle.state != .closed
        else { return }
        if case .failed = session.lifecycle.state { return }
        session.lifecycle.nativeFailure(generation: advertiseId, message: message)
        NSLog("[iroh-http-dnssd] advertise \(advertiseId) failed: \(message)")
        requestAdvertisementStop(session)
    }

    private func advertisementDidStop(_ advertiseId: UInt64) {
        guard let session = advertiseSessions[advertiseId] else { return }
        let stateBeforeStop = session.lifecycle.state
        let startWasPending = session.lifecycle.startCompletion == .pending
        session.lifecycle.nativeStopped(generation: advertiseId)
        if startWasPending, case .rejected(let message) = session.lifecycle.startCompletion {
            if case .failed = stateBeforeStop {
                session.startInvoke.reject("Failed to publish DNS-SD service: \(message)")
            } else {
                session.startInvoke.reject("DNS-SD \(message)")
            }
        }
        let completions = session.pendingStopCompletions
        session.pendingStopCompletions = []
        advertiseSessions.removeValue(forKey: advertiseId)
        for completion in completions {
            completion.resolve()
        }
        finalizeAdvertisementService(session)
    }

    private func startAdvertisement(
        invoke: Invoke,
        advertiseId: UInt64,
        port: UInt16,
        txtData: Data,
        makeService: @escaping () -> NetService
    ) {
        let startInvoke = InvokeOnce(invoke)
        DispatchQueue.main.async {
            // NetService auto-schedules exactly once on the creating thread's
            // current/default run loop. Construct on main rather than creating
            // on the Tauri IPC queue and adding a second registration later.
            dispatchPrecondition(condition: .onQueue(DispatchQueue.main))
            let service = makeService()
            let registrationDelegate = NetServiceRegistrationDelegate(
                onPublished: { [weak self] in
                    self?.queue.async { self?.advertisementDidPublish(advertiseId) }
                },
                onFailure: { [weak self] message in
                    self?.queue.async { self?.advertisementDidFail(advertiseId, message: message) }
                },
                onStopped: { [weak self] in
                    self?.queue.async { self?.advertisementDidStop(advertiseId) }
                }
            )
            let session = AdvertiseSession(
                id: advertiseId,
                service: service,
                registrationDelegate: registrationDelegate,
                port: port,
                startInvoke: startInvoke
            )
            service.delegate = registrationDelegate

            self.queue.async {
                // Publish is dispatched only after the serial registry owns the
                // session, so even an immediate callback finds its exact state.
                self.advertiseSessions[advertiseId] = session
                DispatchQueue.main.async {
                    dispatchPrecondition(condition: .onQueue(DispatchQueue.main))
                    guard service.setTXTRecord(txtData) else {
                        self.queue.async {
                            self.advertisementDidFail(
                                advertiseId,
                                message: "NetService rejected the TXT record"
                            )
                        }
                        return
                    }
                    service.publish(options: .noAutoRename)
                }
            }
        }
    }

    private func stopAdvertisement(
        _ advertiseId: UInt64,
        invoke: Invoke
    ) {
        let completion = InvokeOnce(invoke)
        queue.async {
            guard let session = self.advertiseSessions[advertiseId] else {
                completion.resolve()
                return
            }
            let disposition = session.lifecycle.requestStop()
            session.pendingStopCompletions.append(completion)
            if disposition == .afterUpdate {
                return
            }
            if disposition == .alreadyStopping {
                return
            }
            if disposition == .alreadyStopped {
                session.pendingStopCompletions.removeLast()
                completion.resolve()
                return
            }
            self.requestAdvertisementStop(session)
        }
    }

    private func finishDeferredAdvertisementStop(_ session: AdvertiseSession) {
        requestAdvertisementStop(session)
    }

    /// Apply a TXT-only mutation to an existing native registration. Both
    /// public advertisement adapters share this serialization point so a stop
    /// cannot overtake the main-thread NetService update.
    private func updateAdvertisementTxt(
        _ session: AdvertiseSession,
        advertiseId: UInt64,
        encodingFailurePrefix: String,
        makeTxtData: () throws -> Data,
        completion: InvokeOnce
    ) {
        let displayName = "DNS-SD"
        guard session.lifecycle.state == .active else {
            if case .failed(let message) = session.lifecycle.state {
                completion.reject("\(displayName) advertisement failed: \(message)")
            } else {
                completion.reject("\(displayName) advertisement is not active")
            }
            return
        }

        let txtData: Data
        do {
            txtData = try makeTxtData()
        } catch {
            completion.reject("\(encodingFailurePrefix): \(error.localizedDescription)")
            return
        }
        guard session.lifecycle.beginUpdate(generation: advertiseId) else {
            completion.reject("\(displayName) advertisement update is already in progress")
            return
        }

        DispatchQueue.main.async {
            dispatchPrecondition(condition: .onQueue(DispatchQueue.main))
            let didUpdate = session.service.setTXTRecord(txtData)
            self.queue.async {
                guard self.advertiseSessions[advertiseId] === session else {
                    completion.reject("\(displayName) advertisement is closed")
                    if session.lifecycle.finishUpdate(generation: advertiseId) {
                        self.finishDeferredAdvertisementStop(session)
                    }
                    return
                }
                if session.lifecycle.state != .active {
                    let reason: String
                    if case .failed(let message) = session.lifecycle.state {
                        reason = message
                    } else {
                        reason = "not active"
                    }
                    completion.reject("\(displayName) advertisement failed: \(reason)")
                } else if didUpdate {
                    completion.resolve()
                } else {
                    completion.reject(
                        "NetService rejected the updated generic DNS-SD TXT record"
                    )
                }
                if session.lifecycle.finishUpdate(generation: advertiseId) {
                    self.finishDeferredAdvertisementStop(session)
                }
            }
        }
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
        let startInvoke = InvokeOnce(invoke)

        let proto = args.`protocol`.lowercased() == "tcp" ? "_tcp" : "_udp"
        let serviceType = "_\(args.serviceName).\(proto)"
        let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(type: serviceType, domain: nil)
        let browser = NWBrowser(for: descriptor, using: parameters(for: args.`protocol`))
        let session = DnsSdBrowseSession(
            id: browseId,
            browser: browser,
            serviceType: serviceType,
            startInvoke: startInvoke
        )

        browser.browseResultsChangedHandler = { [weak self, weak session] latestResults, _ in
            guard let self = self, let session = session else { return }
            self.queue.async {
                self.handleDnsSdBrowseResults(session: session, results: latestResults)
            }
        }

        browser.stateUpdateHandler = { [weak self, weak session] state in
            guard let self = self, let session = session else { return }
            self.queue.async {
                self.handleDnsSdBrowseState(session: session, state: state)
            }
        }

        queue.async {
            self.dnsSdBrowseSessions[browseId] = session
            browser.start(queue: self.queue)
        }
    }

    private func handleDnsSdBrowseState(session: DnsSdBrowseSession, state: NWBrowser.State) {
        guard dnsSdBrowseSessions[session.id] === session else { return }
        switch state {
        case .ready:
            if session.lifecycle.nativeReady(generation: session.id) {
                session.startInvoke.resolve(["browseId": session.id])
            }
        case .failed(let error):
            let message = error.localizedDescription
            session.lifecycle.nativeFailure(generation: session.id, message: message)
            if case .dns(let code) = error, code == -65569 {
                // Quiet expected duplicate/teardown failures in logs only.
            } else {
                NSLog("[iroh-http-dnssd] browse \(session.id) failed: \(message)")
            }
            session.browser.cancel()
        case .cancelled:
            let stateBeforeCancellation = session.lifecycle.state
            let startWasPending = session.lifecycle.startCompletion == .pending
            session.lifecycle.nativeCancelled(generation: session.id)
            if startWasPending, case .rejected(let message) = session.lifecycle.startCompletion {
                if case .failed = stateBeforeCancellation {
                    session.startInvoke.reject("Failed to start DNS-SD browse: \(message)")
                } else {
                    session.startInvoke.reject("DNS-SD \(message)")
                }
            }
            let completions = session.pendingStopCompletions
            session.pendingStopCompletions = []
            for completion in completions {
                completion.resolve()
            }
            session.browser.browseResultsChangedHandler = nil
            session.browser.stateUpdateHandler = nil
            if !completions.isEmpty || session.lifecycle.state == .closed {
                dnsSdBrowseSessions.removeValue(forKey: session.id)
            }
        default:
            break
        }
    }

    /// Build generic DNS-SD records from browse results.
    ///
    /// Note: `NWBrowser` surfaces the instance name, service type and TXT record
    /// but does *not* resolve the target host, port or IP addresses — that would
    /// require opening an `NWConnection` per result. iOS records are therefore
    /// metadata-only (`host = nil`, `port = 0`, `addrs = []`). Reserved peer
    /// properties such as `relay` and `address` remain ordinary TXT entries.
    private func handleDnsSdBrowseResults(
        session: DnsSdBrowseSession, results: Set<NWBrowser.Result>
    ) {
        guard
            session.lifecycle.state == .starting || session.lifecycle.state == .active
        else { return }
        var records: [DiscoveryDnsSdRecord] = []

        for result in results {
            guard case .service(let name, _, _, _) = result.endpoint else { continue }

            var txt: [String: String] = [:]
            if case .bonjour(let txtRecord) = result.metadata {
                txt = decodeTxtRecord(txtRecord)
            }
            records.append(
                DiscoveryDnsSdRecord(
                    serviceType: session.serviceType,
                    instanceName: name,
                    txt: txt
                )
            )
        }

        let changes = session.lifecycle.nativeSnapshot(
            generation: session.id,
            records: records
        )
        for change in changes where change.record.isActive {
            // #334: greppable trace of the re-emit dedup. A known instance whose
            // TXT changed re-surfaces here (`event=reemit`) instead of
            // being suppressed forever by a one-shot Set. `rev` mirrors the
            // example app's mutate counter when present.
            NSLog(
                "IROH_DNSSD_CHECK reemit instance=\(change.record.instanceName) event=\(change.isUpdate ? "reemit" : "new") rev=\(change.record.txt["rev"] ?? "-")"
            )
        }
    }

    @objc public func browse_poll(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowsePollArgs.self)
        queue.async {
            guard let session = self.dnsSdBrowseSessions[args.browseId] else {
                invoke.resolve([
                    "status": "closed",
                    "records": [] as [[String: Any]],
                ])
                return
            }
            let poll = session.lifecycle.poll()
            let records: [[String: Any]] = poll.records.map { record in
                [
                    "isActive": record.isActive,
                    "serviceType": record.serviceType,
                    "instanceName": record.instanceName,
                    "host": NSNull(),
                    "port": 0,
                    "addrs": [String](),
                    "txt": record.txt,
                ]
            }
            var payload: [String: Any] = [
                "status": poll.status,
                "records": records,
            ]
            if let error = poll.error { payload["error"] = error }
            invoke.resolve(payload)
            if (poll.status == "closed" || poll.status == "failed")
                && session.lifecycle.nativeTerminalAcknowledged
            {
                self.dnsSdBrowseSessions.removeValue(forKey: args.browseId)
            }
        }
    }

    @objc public func browse_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(BrowseStopArgs.self)
        let completion = InvokeOnce(invoke)
        queue.async {
            if let session = self.dnsSdBrowseSessions[args.browseId] {
                let disposition = session.lifecycle.requestStop()
                session.pendingStopCompletions.append(completion)
                if disposition == .alreadyTerminal {
                    session.pendingStopCompletions.removeLast()
                    completion.resolve()
                    if session.lifecycle.state == .closed {
                        self.dnsSdBrowseSessions.removeValue(forKey: args.browseId)
                    }
                    return
                }
                if disposition == .alreadyStopping {
                    return
                }
                session.browser.browseResultsChangedHandler = nil
                session.browser.cancel()
            } else {
                completion.resolve()
            }
        }
    }

    @objc public func advertise_start(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(DnsSdAdvertiseStartArgs.self)
        guard args.addrs.isEmpty else {
            invoke.reject(
                "iOS DNS-SD advertisement cannot publish explicit addrs; "
                    + "omit addrs to advertise the device's current interface addresses"
            )
            return
        }
        guard args.port != 0 else {
            invoke.reject("Cannot advertise a generic DNS-SD service on port 0")
            return
        }
        guard
            let instanceData = args.instanceName.data(using: .utf8),
            !instanceData.isEmpty,
            instanceData.count <= 63
        else {
            invoke.reject("DNS-SD instanceName must contain 1...63 UTF-8 bytes")
            return
        }
        let advertiseId = allocAdvertiseId()

        let proto = args.`protocol`.lowercased() == "tcp" ? "_tcp" : "_udp"
        let serviceType = "_\(args.serviceName).\(proto)."
        let txtData: Data
        do {
            txtData = try encodeTxtData(args.txt)
        } catch {
            invoke.reject("Failed to encode generic DNS-SD TXT: \(error.localizedDescription)")
            return
        }

        // NetService registers the caller-owned service's existing port. It
        // does not bind that port and therefore cannot intercept or blackhole
        // incoming connections (#366). Bonjour derives local A/AAAA records
        // when explicit `addrs` is empty; TXT is retained field-for-field.
        startAdvertisement(
            invoke: invoke,
            advertiseId: advertiseId,
            port: args.port,
            txtData: txtData,
            makeService: {
                NetService(
                    domain: "local.",
                    type: serviceType,
                    name: args.instanceName,
                    port: Int32(args.port)
                )
            }
        )
    }

    @objc public func advertise_update(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(DnsSdAdvertiseUpdateArgs.self)
        let completion = InvokeOnce(invoke)

        if let rejection = DiscoveryAdvertisementUpdatePolicy.rejection(
            publishedPort: args.port,
            proposedPort: args.port,
            hasExplicitAddrs: !args.addrs.isEmpty
        ) {
            completion.reject(rejection)
            return
        }

        queue.async {
            guard let session = self.advertiseSessions[args.advertiseId] else {
                completion.reject("DNS-SD advertisement is closed")
                return
            }
            if let rejection = DiscoveryAdvertisementUpdatePolicy.rejection(
                publishedPort: session.port,
                proposedPort: args.port,
                hasExplicitAddrs: false
            ) {
                completion.reject(rejection)
                return
            }

            self.updateAdvertisementTxt(
                session,
                advertiseId: args.advertiseId,
                encodingFailurePrefix: "Failed to encode generic DNS-SD TXT",
                makeTxtData: { try self.encodeTxtData(args.txt) },
                completion: completion
            )
        }
    }

    @objc public func advertise_stop(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(AdvertiseStopArgs.self)
        stopAdvertisement(args.advertiseId, invoke: invoke)
    }

    /// Convert one POSIX interface address to a numeric IP literal. Conversion
    /// failures are isolated to the current entry so one malformed sockaddr
    /// cannot hide otherwise usable VPN/LAN candidates.
    private func numericInterfaceAddress(_ address: UnsafePointer<sockaddr>) -> String? {
        let family = Int32(address.pointee.sa_family)
        guard family == AF_INET || family == AF_INET6 else { return nil }

        var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        let status = getnameinfo(
            address,
            socklen_t(address.pointee.sa_len),
            &host,
            socklen_t(host.count),
            nil,
            0,
            NI_NUMERICHOST | NI_NUMERICSCOPE
        )
        guard status == 0 else { return nil }

        // Interface inventory crosses to Rust as `IpAddr` strings. A scoped
        // link-local value may include `%<index>`; retain the IP literal only.
        // Rust performs the final routability filter and drops link-local IPs.
        let rendered = String(cString: host)
        let literal = String(rendered.split(separator: "%", maxSplits: 1)[0])
        if family == AF_INET {
            guard let ip = IPv4Address(literal), !ip.isLoopback else { return nil }
        } else {
            guard let ip = IPv6Address(literal), !ip.isLoopback else { return nil }
        }
        return literal
    }

    /// Enumerate operational non-loopback interface addresses for mobile
    /// direct-address fallback. Rust applies the shared routability policy;
    /// native code is responsible only for safe platform collection.
    @objc public func get_interface_addresses(_ invoke: Invoke) throws {
        var interfaces: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&interfaces) == 0 else {
            invoke.reject("Failed to enumerate iOS interface addresses: \(String(cString: strerror(errno)))")
            return
        }
        defer { freeifaddrs(interfaces) }

        var addresses: Set<String> = []
        var cursor = interfaces
        while let interface = cursor {
            defer { cursor = interface.pointee.ifa_next }
            let flags = interface.pointee.ifa_flags
            guard
                flags & UInt32(IFF_UP) != 0,
                flags & UInt32(IFF_RUNNING) != 0,
                flags & UInt32(IFF_LOOPBACK) == 0,
                let address = interface.pointee.ifa_addr,
                let literal = numericInterfaceAddress(address)
            else { continue }
            addresses.insert(literal)
        }

        invoke.resolve(["addresses": addresses.sorted()])
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
