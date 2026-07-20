import Foundation

/// Platform-neutral lifecycle state shared by the production DNS-SD native
/// adapters and the host-side callback contract.
enum DiscoverySessionState: Equatable {
    case starting
    case active
    case stopping
    case closed
    case failed(String)

    var pollValue: String {
        switch self {
        case .starting, .active, .stopping:
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

/// Invoke-equivalent completion of an accepted native stop. Requesting a stop
/// never resolves it; only the platform's terminal callback may do that.
enum DiscoveryStopCompletion: Equatable {
    case notRequested
    case pending
    case resolved
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

enum DiscoveryBrowseStopDisposition: Equatable {
    case cancelNow
    case alreadyStopping
    case alreadyTerminal
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
    private(set) var stopCompletion: DiscoveryStopCompletion = .notRequested
    private(set) var nativeTerminalAcknowledged = false

    private var known: [String: DiscoveryDnsSdRecord] = [:]
    private var pending: [DiscoveryDnsSdRecord] = []
    private var pendingStartFailure: String?

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
            pendingStartFailure = message
        case .active:
            state = .failed(message)
        case .stopping, .closed, .failed:
            return
        }
    }

    func nativeCancelled(generation: UInt64) {
        guard generation == callbackGeneration else { return }
        nativeTerminalAcknowledged = true
        switch state {
        case .starting:
            state = .closed
            completeStart(.rejected("browse closed before becoming ready"))
        case .active:
            state = .closed
        case .stopping:
            state = .closed
            if startCompletion == .pending {
                completeStart(
                    .rejected(pendingStartFailure ?? "browse closed before becoming ready")
                )
            }
            stopCompletion = .resolved
        case .failed(let message):
            if startCompletion == .pending {
                completeStart(.rejected(pendingStartFailure ?? message))
                state = .closed
            }
        case .closed:
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
    func requestStop() -> DiscoveryBrowseStopDisposition {
        if nativeTerminalAcknowledged {
            // Stop won the serial-queue race with poll, so it becomes the
            // terminal consumer. Discard an unobserved failure and make the
            // native session removable without waiting for another command.
            state = .closed
            stopCompletion = .resolved
            known.removeAll()
            pending.removeAll()
            return .alreadyTerminal
        }
        if state == .stopping {
            return .alreadyStopping
        }
        if state == .starting {
            pendingStartFailure = "browse closed before becoming ready"
        }
        stopCompletion = .pending
        state = .stopping
        known.removeAll()
        pending.removeAll()
        return .cancelNow
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
    case alreadyStopping
    case alreadyStopped
}

/// iOS `NetService` can change TXT on a live registration, but its published
/// port and automatically selected interface addresses are immutable. Keeping
/// this validation beside the lifecycle reducer makes an update either a pure
/// TXT mutation or an explicit rejection that preserves service identity.
enum DiscoveryAdvertisementUpdatePolicy {
    static func rejection(
        publishedPort: UInt16,
        proposedPort: UInt16,
        hasExplicitAddrs: Bool
    ) -> String? {
        if hasExplicitAddrs {
            return "iOS DNS-SD advertisement cannot publish explicit addrs; "
                + "omit addrs to advertise the device's current interface addresses"
        }
        if proposedPort == 0 {
            return "Cannot update a generic DNS-SD service to port 0"
        }
        if proposedPort != publishedPort {
            return "iOS DNS-SD advertisement port cannot change from \(publishedPort) "
                + "to \(proposedPort); close and restart the advertisement"
        }
        return nil
    }
}

/// Deterministic readiness/terminal reducer for one native advertisement.
final class DiscoveryAdvertisementLifecycle {
    let id: UInt64
    let callbackGeneration: UInt64

    private(set) var state: DiscoverySessionState = .starting
    private(set) var startCompletion: DiscoveryStartCompletion = .pending
    private(set) var stopCompletion: DiscoveryStopCompletion = .notRequested
    private(set) var nativeTerminalAcknowledged = false
    private var updateInFlight = false
    private var stopAfterUpdate = false
    private var pendingStartFailure: String?

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
            pendingStartFailure = message
        case .active:
            state = .failed(message)
        case .stopping, .closed, .failed:
            return
        }
    }

    func nativeStopped(generation: UInt64) {
        guard generation == callbackGeneration else { return }
        nativeTerminalAcknowledged = true
        switch state {
        case .starting:
            let message = "registration stopped before it was published"
            state = .failed(message)
            completeStart(.rejected(message))
        case .active:
            state = .failed("registration stopped unexpectedly")
        case .stopping:
            state = .closed
            if startCompletion == .pending {
                completeStart(
                    .rejected(pendingStartFailure ?? "advertisement closed before becoming ready")
                )
            }
            stopCompletion = .resolved
        case .failed(let message):
            state = .closed
            if startCompletion == .pending {
                completeStart(.rejected(pendingStartFailure ?? message))
            }
        case .closed:
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
        if state == .stopping {
            return .alreadyStopping
        }
        stopCompletion = .pending
        if state == .starting {
            pendingStartFailure = "advertisement closed before becoming ready"
        }
        if updateInFlight {
            stopAfterUpdate = true
            return .afterUpdate
        }
        state = .stopping
        return .stopNow
    }

    /// Finish the current native update. Returns `true` when a stop was waiting
    /// and the caller must now tear down the platform registration.
    func finishUpdate(generation: UInt64) -> Bool {
        guard generation == callbackGeneration, updateInFlight else { return false }
        updateInFlight = false
        guard stopAfterUpdate else { return false }
        stopAfterUpdate = false
        state = .stopping
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
