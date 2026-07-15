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
        try advertisementUpdateFinishesBeforeStop()
        try genericAdvertisementUpdatePreservesIdentity()
        print("iOS generic DNS-SD lifecycle contract passed")
    }

    private static func browseReadinessAndFailureAreOneShot() throws {
        let ready = DiscoveryBrowseLifecycle(id: 7, callbackGeneration: 11)
        try lifecycleRequire(ready.startCompletion == .pending, "browse start resolved before ready")
        ready.nativeReady(generation: 11)
        try lifecycleRequire(ready.startCompletion == .resolved(7), "ready did not resolve browse start")
        ready.nativeFailure(generation: 11, message: "late failure")
        try lifecycleRequire(ready.startCompletion == .resolved(7), "browse start completed more than once")
        try lifecycleRequire(!ready.nativeReady(generation: 11), "duplicate ready completed browse start twice")

        let failed = DiscoveryBrowseLifecycle(id: 8, callbackGeneration: 12)
        failed.nativeFailure(generation: 12, message: "permission denied")
        failed.nativeReady(generation: 12)
        try lifecycleRequire(
            failed.startCompletion == .rejected("permission denied"),
            "failure-before-ready did not reject browse start"
        )
        try lifecycleRequire(
            failed.startCompletion == .rejected("permission denied"),
            "failed browse start completed twice"
        )

        let activeFailure = DiscoveryBrowseLifecycle(id: 9, callbackGeneration: 13)
        activeFailure.nativeReady(generation: 13)
        activeFailure.nativeFailure(generation: 13, message: "network changed")
        let terminal = activeFailure.poll()
        try lifecycleRequire(terminal.status == "failed", "active browse failure was not observable")
        try lifecycleRequire(terminal.error == "network changed", "browse failure lost its reason")
        try lifecycleRequire(activeFailure.poll().status == "closed", "failure was not consumed once")
    }

    private static func browseFoundChangedAndLostAreObservable() throws {
        let browse = DiscoveryBrowseLifecycle(id: 10, callbackGeneration: 20)
        browse.nativeReady(generation: 20)

        let first = DiscoveryDnsSdRecord(
            serviceType: "_demo._udp",
            instanceName: "printer",
            txt: ["rev": "1", "address": "192.0.2.10:8080"]
        )
        browse.nativeSnapshot(generation: 20, records: [first])
        try lifecycleRequire(browse.poll().records == [first], "found instance did not emit an upsert")

        browse.nativeSnapshot(generation: 20, records: [first])
        try lifecycleRequire(browse.poll().records.isEmpty, "unchanged instance was re-emitted")

        let changed = DiscoveryDnsSdRecord(
            serviceType: "_demo._udp",
            instanceName: "printer",
            txt: ["rev": "2", "address": "192.0.2.11:8080"]
        )
        browse.nativeSnapshot(generation: 20, records: [changed])
        try lifecycleRequire(browse.poll().records == [changed], "changed instance was suppressed")

        let conflicting = DiscoveryDnsSdRecord(
            serviceType: "_demo._udp",
            instanceName: "printer",
            txt: ["rev": "3", "zone": "z"]
        )
        browse.nativeSnapshot(generation: 20, records: [conflicting, changed])
        let selected = [conflicting, changed].min { $0.stableOrderingKey < $1.stableOrderingKey }!
        try lifecycleRequire(
            browse.poll().records == (selected == changed ? [] : [selected]),
            "duplicate instance rows were not coalesced deterministically"
        )
        browse.nativeSnapshot(generation: 20, records: [changed, conflicting])
        try lifecycleRequire(
            browse.poll().records.isEmpty,
            "reordered duplicate instance rows oscillated the snapshot"
        )

        browse.nativeSnapshot(generation: 20, records: [])
        try lifecycleRequire(
            browse.poll().records == [selected.inactive()],
            "lost instance did not emit an inactive record"
        )
    }

    private static func browseStopRetiresCallbacks() throws {
        let browse = DiscoveryBrowseLifecycle(id: 11, callbackGeneration: 30)

        browse.nativeReady(generation: 29)
        try lifecycleRequire(browse.startCompletion == .pending, "stale generation resolved browse start")
        try lifecycleRequire(browse.stop(), "browse stop was not acknowledged")
        try lifecycleRequire(
            browse.startCompletion == .rejected("browse closed before becoming ready"),
            "stop-before-ready did not reject the pending start"
        )

        let late = DiscoveryDnsSdRecord(serviceType: "_demo._udp", instanceName: "late")
        browse.nativeReady(generation: 30)
        browse.nativeSnapshot(generation: 30, records: [late])
        browse.nativeFailure(generation: 30, message: "late failure")
        browse.nativeCancelled(generation: 30)
        try lifecycleRequire(browse.state == .closed, "late callback revived a stopped browse")
        try lifecycleRequire(browse.poll().records.isEmpty, "late callback enqueued after stop")
        try lifecycleRequire(
            browse.startCompletion == .rejected("browse closed before becoming ready"),
            "close race completed browse start twice"
        )

        try lifecycleRequire(browse.stop(), "idempotent browse stop was not acknowledged")
        try lifecycleRequire(browse.state == .closed, "idempotent stop changed terminal state")
    }

    private static func advertisementAcknowledgementsAndRaces() throws {
        let ready = DiscoveryAdvertisementLifecycle(id: 21, callbackGeneration: 40)
        try lifecycleRequire(ready.startCompletion == .pending, "advertise start resolved before publish")
        ready.nativePublished(generation: 40)
        try lifecycleRequire(ready.startCompletion == .resolved(21), "publish did not resolve advertise start")
        try lifecycleRequire(ready.requestStop() == .stopNow, "advertise stop was not immediate")
        ready.nativeStopped(generation: 40)
        ready.nativeFailure(generation: 40, message: "late failure")
        try lifecycleRequire(ready.state == .closed, "late callback revived stopped advertisement")
        try lifecycleRequire(ready.startCompletion == .resolved(21), "advertise start completed twice")

        let failed = DiscoveryAdvertisementLifecycle(id: 22, callbackGeneration: 41)
        failed.nativeFailure(generation: 41, message: "name conflict")
        failed.nativePublished(generation: 41)
        try lifecycleRequire(
            failed.startCompletion == .rejected("name conflict"),
            "failure-before-publish did not reject advertise start"
        )
        try lifecycleRequire(
            failed.startCompletion == .rejected("name conflict"),
            "failed advertise start completed twice"
        )

        let stoppedWhileStarting = DiscoveryAdvertisementLifecycle(id: 23, callbackGeneration: 42)
        try lifecycleRequire(
            stoppedWhileStarting.requestStop() == .stopNow,
            "stop-before-publish was not immediate"
        )
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
            stoppedWhileStarting.startCompletion
                == .rejected("advertisement closed before becoming ready"),
            "publish/stop race completed advertise start twice"
        )
    }

    private static func advertisementUpdateFinishesBeforeStop() throws {
        let advertisement = DiscoveryAdvertisementLifecycle(id: 24, callbackGeneration: 43)
        advertisement.nativePublished(generation: 43)

        try lifecycleRequire(
            advertisement.beginUpdate(generation: 43),
            "active advertisement did not begin its update"
        )
        try lifecycleRequire(
            advertisement.requestStop() == .afterUpdate,
            "stop did not wait for the in-flight update"
        )
        try lifecycleRequire(
            advertisement.state == .active,
            "stop closed the advertisement before its update completed"
        )
        try lifecycleRequire(
            advertisement.finishUpdate(generation: 43),
            "finishing the update did not release the deferred stop"
        )
        try lifecycleRequire(
            advertisement.state == .closed,
            "advertisement remained active after update-before-stop completed"
        )

        advertisement.nativeFailure(generation: 43, message: "late failure")
        try lifecycleRequire(
            advertisement.state == .closed,
            "late update/stop callback revived the advertisement"
        )
    }

    private static func genericAdvertisementUpdatePreservesIdentity() throws {
        try lifecycleRequire(
            DiscoveryAdvertisementUpdatePolicy.rejection(
                publishedPort: 8080,
                proposedPort: 8080,
                hasExplicitAddrs: false
            ) == nil,
            "TXT-only update on the published port was rejected"
        )
        try lifecycleRequire(
            DiscoveryAdvertisementUpdatePolicy.rejection(
                publishedPort: 8080,
                proposedPort: 0,
                hasExplicitAddrs: false
            ) != nil,
            "port-zero update was accepted"
        )
        try lifecycleRequire(
            DiscoveryAdvertisementUpdatePolicy.rejection(
                publishedPort: 8080,
                proposedPort: 8080,
                hasExplicitAddrs: true
            ) != nil,
            "explicit-address update was accepted"
        )
        try lifecycleRequire(
            DiscoveryAdvertisementUpdatePolicy.rejection(
                publishedPort: 8080,
                proposedPort: 9090,
                hasExplicitAddrs: false
            ) != nil,
            "immutable NetService port change was accepted"
        )
    }
}
