import Foundation

/// Executable specification for the platform-neutral portion of the native
/// DNS-SD lifecycle. The production plugin currently keeps this state inside
/// `IrohHttpPlugin`, where `NWBrowser`, `NetService`, and Tauri's `Invoke` make
/// it impossible to drive deterministically from a host-side Swift program.
///
/// The refactor should replace this test-only model with the extracted
/// production reducer without changing the contract cases below.
enum ContractSessionState: Equatable {
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

enum ContractStartCompletion: Equatable {
    case pending
    case resolved(UInt64)
    case rejected(String)
}

struct ContractDnsSdRecord: Equatable {
    let serviceType: String
    let instanceName: String
    let txt: [String: String]
    let addrs: [String]
    let isActive: Bool

    init(
        serviceType: String = "_demo._udp",
        instanceName: String,
        txt: [String: String] = [:],
        addrs: [String] = [],
        isActive: Bool = true
    ) {
        self.serviceType = serviceType
        self.instanceName = instanceName
        self.txt = txt
        self.addrs = addrs
        self.isActive = isActive
    }

    func inactive() -> ContractDnsSdRecord {
        ContractDnsSdRecord(
            serviceType: serviceType,
            instanceName: instanceName,
            isActive: false
        )
    }
}

struct ContractBrowsePoll: Equatable {
    let status: String
    let records: [ContractDnsSdRecord]
    let error: String?
}

/// A deterministic browse lifecycle with the same externally visible rules as
/// the generic iOS commands: readiness-gated start, snapshot-based found/lost
/// events, one-shot terminal polling, and callback-generation isolation.
final class ContractBrowseLifecycle {
    let id: UInt64
    let callbackGeneration: UInt64

    private(set) var state: ContractSessionState = .starting
    private(set) var startCompletion: ContractStartCompletion = .pending
    private(set) var startCompletionCount = 0
    private(set) var stopAcknowledgementCount = 0

    private var known: [String: ContractDnsSdRecord] = [:]
    private var pending: [ContractDnsSdRecord] = []

    init(id: UInt64, callbackGeneration: UInt64) {
        self.id = id
        self.callbackGeneration = callbackGeneration
    }

    func nativeReady(generation: UInt64) {
        guard accepts(generation), state == .starting else { return }
        state = .active
        completeStart(.resolved(id))
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

    func nativeSnapshot(generation: UInt64, records: [ContractDnsSdRecord]) {
        guard accepts(generation) else { return }
        guard state == .starting || state == .active else { return }

        var current: Set<String> = []
        for record in records where record.isActive {
            current.insert(record.instanceName)
            if known[record.instanceName] != record {
                known[record.instanceName] = record
                pending.append(record)
            }
        }

        for name in Set(known.keys).subtracting(current).sorted() {
            if let expired = known.removeValue(forKey: name) {
                pending.append(expired.inactive())
            }
        }
    }

    @discardableResult
    func stop() -> Bool {
        stopAcknowledgementCount += 1
        if state == .starting {
            completeStart(.rejected("browse closed before becoming ready"))
        }
        state = .closed
        known.removeAll()
        pending.removeAll()
        return true
    }

    func poll() -> ContractBrowsePoll {
        let records = pending
        pending.removeAll()

        switch state {
        case .failed(let message):
            state = .closed
            return ContractBrowsePoll(status: "failed", records: records, error: message)
        default:
            return ContractBrowsePoll(status: state.pollValue, records: records, error: nil)
        }
    }

    private func accepts(_ generation: UInt64) -> Bool {
        generation == callbackGeneration && state != .closed
    }

    private func completeStart(_ completion: ContractStartCompletion) {
        guard startCompletion == .pending else { return }
        startCompletion = completion
        startCompletionCount += 1
    }
}

/// Deterministic advertisement lifecycle for NetService publish/failure/stop
/// callback races. Explicit stop acknowledges immediately and retires the
/// callback generation; late native callbacks cannot revive the session.
final class ContractAdvertiseLifecycle {
    let id: UInt64
    let callbackGeneration: UInt64

    private(set) var state: ContractSessionState = .starting
    private(set) var startCompletion: ContractStartCompletion = .pending
    private(set) var startCompletionCount = 0
    private(set) var stopAcknowledgementCount = 0

    init(id: UInt64, callbackGeneration: UInt64) {
        self.id = id
        self.callbackGeneration = callbackGeneration
    }

    func nativePublished(generation: UInt64) {
        guard accepts(generation), state == .starting else { return }
        state = .active
        completeStart(.resolved(id))
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
            state = .failed("registration stopped before it was published")
            completeStart(.rejected("registration stopped before it was published"))
        case .active:
            state = .failed("registration stopped unexpectedly")
        case .closed, .failed:
            return
        }
    }

    @discardableResult
    func stop() -> Bool {
        stopAcknowledgementCount += 1
        if state == .starting {
            completeStart(.rejected("advertisement closed before becoming ready"))
        }
        state = .closed
        return true
    }

    private func accepts(_ generation: UInt64) -> Bool {
        generation == callbackGeneration && state != .closed
    }

    private func completeStart(_ completion: ContractStartCompletion) {
        guard startCompletion == .pending else { return }
        startCompletion = completion
        startCompletionCount += 1
    }
}
