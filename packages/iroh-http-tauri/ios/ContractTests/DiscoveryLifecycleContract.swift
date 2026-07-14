private enum LifecycleContractFailure: Error {
    case failed(String)
}

private func lifecycleRequire(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    guard condition() else { throw LifecycleContractFailure.failed(message) }
}

@main
private struct DiscoveryLifecycleContract {
    static func main() throws {
        try browseReadinessAndFailureAreOneShot()
        try browseFoundChangedAndLostAreObservable()
        try browseStopRetiresCallbacks()
        try advertisementAcknowledgementsAndRaces()
        print("iOS generic DNS-SD lifecycle contract passed")
    }

    private static func browseReadinessAndFailureAreOneShot() throws {
        let ready = ContractBrowseLifecycle(id: 7, callbackGeneration: 11)
        try lifecycleRequire(ready.startCompletion == .pending, "browse start resolved before ready")
        ready.nativeReady(generation: 11)
        try lifecycleRequire(ready.startCompletion == .resolved(7), "ready did not resolve browse start")
        ready.nativeFailure(generation: 11, message: "late failure")
        try lifecycleRequire(ready.startCompletionCount == 1, "browse start completed more than once")

        let failed = ContractBrowseLifecycle(id: 8, callbackGeneration: 12)
        failed.nativeFailure(generation: 12, message: "permission denied")
        failed.nativeReady(generation: 12)
        try lifecycleRequire(
            failed.startCompletion == .rejected("permission denied"),
            "failure-before-ready did not reject browse start"
        )
        try lifecycleRequire(failed.startCompletionCount == 1, "failed browse start completed twice")

        let activeFailure = ContractBrowseLifecycle(id: 9, callbackGeneration: 13)
        activeFailure.nativeReady(generation: 13)
        activeFailure.nativeFailure(generation: 13, message: "network changed")
        let terminal = activeFailure.poll()
        try lifecycleRequire(terminal.status == "failed", "active browse failure was not observable")
        try lifecycleRequire(terminal.error == "network changed", "browse failure lost its reason")
        try lifecycleRequire(activeFailure.poll().status == "closed", "failure was not consumed once")
    }

    private static func browseFoundChangedAndLostAreObservable() throws {
        let browse = ContractBrowseLifecycle(id: 10, callbackGeneration: 20)
        browse.nativeReady(generation: 20)

        let first = ContractDnsSdRecord(
            instanceName: "printer",
            txt: ["rev": "1"],
            addrs: ["192.0.2.10:8080"]
        )
        browse.nativeSnapshot(generation: 20, records: [first])
        try lifecycleRequire(browse.poll().records == [first], "found instance did not emit an upsert")

        browse.nativeSnapshot(generation: 20, records: [first])
        try lifecycleRequire(browse.poll().records.isEmpty, "unchanged instance was re-emitted")

        let changed = ContractDnsSdRecord(
            instanceName: "printer",
            txt: ["rev": "2"],
            addrs: ["192.0.2.11:8080"]
        )
        browse.nativeSnapshot(generation: 20, records: [changed])
        try lifecycleRequire(browse.poll().records == [changed], "changed instance was suppressed")

        browse.nativeSnapshot(generation: 20, records: [])
        try lifecycleRequire(
            browse.poll().records == [changed.inactive()],
            "lost instance did not emit an inactive record"
        )
    }

    private static func browseStopRetiresCallbacks() throws {
        let browse = ContractBrowseLifecycle(id: 11, callbackGeneration: 30)

        browse.nativeReady(generation: 29)
        try lifecycleRequire(browse.startCompletion == .pending, "stale generation resolved browse start")
        try lifecycleRequire(browse.stop(), "browse stop was not acknowledged")
        try lifecycleRequire(
            browse.startCompletion == .rejected("browse closed before becoming ready"),
            "stop-before-ready did not reject the pending start"
        )

        let late = ContractDnsSdRecord(instanceName: "late")
        browse.nativeReady(generation: 30)
        browse.nativeSnapshot(generation: 30, records: [late])
        browse.nativeFailure(generation: 30, message: "late failure")
        browse.nativeCancelled(generation: 30)
        try lifecycleRequire(browse.state == .closed, "late callback revived a stopped browse")
        try lifecycleRequire(browse.poll().records.isEmpty, "late callback enqueued after stop")
        try lifecycleRequire(browse.startCompletionCount == 1, "close race completed browse start twice")

        try lifecycleRequire(browse.stop(), "idempotent browse stop was not acknowledged")
        try lifecycleRequire(browse.stopAcknowledgementCount == 2, "each stop call must acknowledge once")
    }

    private static func advertisementAcknowledgementsAndRaces() throws {
        let ready = ContractAdvertiseLifecycle(id: 21, callbackGeneration: 40)
        try lifecycleRequire(ready.startCompletion == .pending, "advertise start resolved before publish")
        ready.nativePublished(generation: 40)
        try lifecycleRequire(ready.startCompletion == .resolved(21), "publish did not resolve advertise start")
        try lifecycleRequire(ready.stop(), "advertise stop was not acknowledged")
        ready.nativeStopped(generation: 40)
        ready.nativeFailure(generation: 40, message: "late failure")
        try lifecycleRequire(ready.state == .closed, "late callback revived stopped advertisement")
        try lifecycleRequire(ready.startCompletionCount == 1, "advertise start completed twice")

        let failed = ContractAdvertiseLifecycle(id: 22, callbackGeneration: 41)
        failed.nativeFailure(generation: 41, message: "name conflict")
        failed.nativePublished(generation: 41)
        try lifecycleRequire(
            failed.startCompletion == .rejected("name conflict"),
            "failure-before-publish did not reject advertise start"
        )
        try lifecycleRequire(failed.startCompletionCount == 1, "failed advertise start completed twice")

        let stoppedWhileStarting = ContractAdvertiseLifecycle(id: 23, callbackGeneration: 42)
        try lifecycleRequire(stoppedWhileStarting.stop(), "stop-before-publish was not acknowledged")
        stoppedWhileStarting.nativePublished(generation: 42)
        stoppedWhileStarting.nativeStopped(generation: 42)
        try lifecycleRequire(
            stoppedWhileStarting.startCompletion
                == .rejected("advertisement closed before becoming ready"),
            "stop-before-publish did not reject pending advertise start"
        )
        try lifecycleRequire(
            stoppedWhileStarting.state == .closed,
            "publish/stop race revived closed advertisement"
        )
        try lifecycleRequire(
            stoppedWhileStarting.startCompletionCount == 1,
            "publish/stop race completed advertise start twice"
        )
    }
}
