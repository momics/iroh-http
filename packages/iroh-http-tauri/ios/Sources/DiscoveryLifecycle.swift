import Foundation

/// Platform-neutral lifecycle state shared by the production DNS-SD native
/// adapters and the host-side callback contract.
enum DiscoverySessionState: Equatable {
    case starting
    case active
    case closed
    case failed(String)

    var pollValue: String {
        switch self {
        case .starting, .active:
            return "active"
        case .closed:
            return "closed"
        case .failed:
            return "failed"
        }
    }
}

/// One-shot result of a readiness-gated native start.
enum DiscoveryStartCompletion: Equatable {
    case pending
    case resolved(UInt64)
    case rejected(String)
}

/// Metadata surfaced by the iOS `NWBrowser` adapter.
///
/// iOS deliberately does not open an `NWConnection` to resolve host, port, or
/// socket addresses. Those fields are supplied as `nil`, `0`, and `[]` by the
/// plugin when it serializes this record for Rust.
struct DiscoveryDnsSdRecord: Equatable {
    let serviceType: String
    let instanceName: String
    let txt: [String: String]
    let isActive: Bool

    init(
        serviceType: String,
        instanceName: String,
        txt: [String: String] = [:],
        isActive: Bool = true
    ) {
        self.serviceType = serviceType
        self.instanceName = instanceName
        self.txt = txt
        self.isActive = isActive
    }

    func inactive() -> DiscoveryDnsSdRecord {
        DiscoveryDnsSdRecord(
            serviceType: serviceType,
            instanceName: instanceName,
            isActive: false
        )
    }

    /// Total ordering used only to collapse duplicate instance rows from the
    /// unordered `NWBrowser.Result` snapshot. DNS-SD identity is the instance;
    /// if interfaces disagree, choosing the same row every time avoids an
    /// oscillating stream while retaining every TXT field from that row.
    var stableOrderingKey: String {
        let fields = txt.keys.sorted().map { key -> String in
            let value = txt[key] ?? ""
            return "\(key.utf8.count):\(key)\(value.utf8.count):\(value)"
        }
        return "\(serviceType.utf8.count):\(serviceType)\(fields.joined())"
    }
}

struct DiscoveryBrowsePoll: Equatable {
    let status: String
    let records: [DiscoveryDnsSdRecord]
    let error: String?
}

struct DiscoveryBrowseChange: Equatable {
    let record: DiscoveryDnsSdRecord
    let isUpdate: Bool
}

/// Deterministic reducer for one native generic browse generation.
///
/// `NWBrowser` callbacks are translated into these methods on the plugin's
/// serial queue. The reducer owns readiness, terminal state, snapshot diffing,
/// and stale-generation rejection; the plugin owns only the browser object,
/// callback buffering across Tauri, and command serialization.
final class DiscoveryBrowseLifecycle {
    let id: UInt64
    let callbackGeneration: UInt64

    private(set) var state: DiscoverySessionState = .starting
    private(set) var startCompletion: DiscoveryStartCompletion = .pending

    private var known: [String: DiscoveryDnsSdRecord] = [:]
    private var pending: [DiscoveryDnsSdRecord] = []

    init(id: UInt64, callbackGeneration: UInt64) {
        self.id = id
        self.callbackGeneration = callbackGeneration
    }

    @discardableResult
    func nativeReady(generation: UInt64) -> Bool {
        guard accepts(generation), state == .starting else { return false }
        state = .active
        return completeStart(.resolved(id))
    }

    func nativeFailure(generation: UInt64, message: String) {
        guard accepts(generation) else { return }
        switch state {
        case .starting:
            state = .failed(message)
            completeStart(.rejected(message))
        case .active:
            state = .failed(message)
        case .closed, .failed:
            return
        }
    }

    func nativeCancelled(generation: UInt64) {
        guard accepts(generation) else { return }
        switch state {
        case .starting:
            state = .closed
            completeStart(.rejected("browse closed before becoming ready"))
        case .active:
            state = .closed
        case .closed, .failed:
            return
        }
    }

    @discardableResult
    func nativeSnapshot(
        generation: UInt64, records: [DiscoveryDnsSdRecord]
    ) -> [DiscoveryBrowseChange] {
        guard accepts(generation) else { return [] }
        guard state == .starting || state == .active else { return [] }

        var byInstance: [String: DiscoveryDnsSdRecord] = [:]
        for record in records where record.isActive {
            if let existing = byInstance[record.instanceName],
                existing.stableOrderingKey <= record.stableOrderingKey
            {
                continue
            }
            byInstance[record.instanceName] = record
        }

        let current = Set(byInstance.keys)
        var changes: [DiscoveryBrowseChange] = []
        for name in current.sorted() {
            guard let record = byInstance[name] else { continue }
            if known[record.instanceName] != record {
                let isUpdate = known[record.instanceName] != nil
                known[record.instanceName] = record
                pending.append(record)
                changes.append(DiscoveryBrowseChange(record: record, isUpdate: isUpdate))
            }
        }

        for name in Set(known.keys).subtracting(current).sorted() {
            if let expired = known.removeValue(forKey: name) {
                let inactive = expired.inactive()
                pending.append(inactive)
                changes.append(DiscoveryBrowseChange(record: inactive, isUpdate: true))
            }
        }
        return changes
    }

    @discardableResult
    func stop() -> Bool {
        if state == .starting {
            completeStart(.rejected("browse closed before becoming ready"))
        }
        state = .closed
        known.removeAll()
        pending.removeAll()
        return true
    }

    func poll() -> DiscoveryBrowsePoll {
        let records = pending
        pending.removeAll()

        switch state {
        case .failed(let message):
            state = .closed
            return DiscoveryBrowsePoll(status: "failed", records: records, error: message)
        default:
            return DiscoveryBrowsePoll(status: state.pollValue, records: records, error: nil)
        }
    }

    private func accepts(_ generation: UInt64) -> Bool {
        generation == callbackGeneration && state != .closed
    }

    @discardableResult
    private func completeStart(_ completion: DiscoveryStartCompletion) -> Bool {
        guard startCompletion == .pending else { return false }
        startCompletion = completion
        return true
    }
}

enum DiscoveryAdvertisementStopDisposition: Equatable {
    case stopNow
    case afterUpdate
    case alreadyStopped
}

/// Deterministic readiness/terminal reducer for one native advertisement.
final class DiscoveryAdvertisementLifecycle {
    let id: UInt64
    let callbackGeneration: UInt64

    private(set) var state: DiscoverySessionState = .starting
    private(set) var startCompletion: DiscoveryStartCompletion = .pending
    private var updateInFlight = false
    private var stopAfterUpdate = false

    init(id: UInt64, callbackGeneration: UInt64) {
        self.id = id
        self.callbackGeneration = callbackGeneration
    }

    @discardableResult
    func nativePublished(generation: UInt64) -> Bool {
        guard accepts(generation), state == .starting else { return false }
        state = .active
        return completeStart(.resolved(id))
    }

    func nativeFailure(generation: UInt64, message: String) {
        guard accepts(generation) else { return }
        switch state {
        case .starting:
            state = .failed(message)
            completeStart(.rejected(message))
        case .active:
            state = .failed(message)
        case .closed, .failed:
            return
        }
    }

    func nativeStopped(generation: UInt64) {
        guard accepts(generation) else { return }
        switch state {
        case .starting:
            let message = "registration stopped before it was published"
            state = .failed(message)
            completeStart(.rejected(message))
        case .active:
            state = .failed("registration stopped unexpectedly")
        case .closed, .failed:
            return
        }
    }

    func beginUpdate(generation: UInt64) -> Bool {
        guard generation == callbackGeneration else { return false }
        guard state == .active, !updateInFlight else { return false }
        updateInFlight = true
        return true
    }

    /// Record an explicit stop without allowing it to overtake an update whose
    /// native callback is still in flight.
    func requestStop() -> DiscoveryAdvertisementStopDisposition {
        if state == .closed {
            return .alreadyStopped
        }
        if state == .starting {
            completeStart(.rejected("advertisement closed before becoming ready"))
        }
        if updateInFlight {
            stopAfterUpdate = true
            return .afterUpdate
        }
        state = .closed
        return .stopNow
    }

    /// Finish the current native update. Returns `true` when a stop was waiting
    /// and the caller must now tear down the platform registration.
    func finishUpdate(generation: UInt64) -> Bool {
        guard generation == callbackGeneration, updateInFlight else { return false }
        updateInFlight = false
        guard stopAfterUpdate else { return false }
        stopAfterUpdate = false
        state = .closed
        return true
    }

    private func accepts(_ generation: UInt64) -> Bool {
        generation == callbackGeneration && state != .closed
    }

    @discardableResult
    private func completeStart(_ completion: DiscoveryStartCompletion) -> Bool {
        guard startCompletion == .pending else { return false }
        startCompletion = completion
        return true
    }
}
