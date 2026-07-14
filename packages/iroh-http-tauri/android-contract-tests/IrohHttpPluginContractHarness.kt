package com.iroh.http

import android.app.Activity
import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import org.json.JSONArray
import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * AOSP moved terminal listener removal across the callback boundary over time:
 * API 21 invokes the callback and then removes the listener, while current
 * Android removes it before dispatching the callback. The plugin must be safe
 * under both orderings.
 */
private enum class TerminalRemovalTiming { BEFORE_CALLBACK, AFTER_CALLBACK }

private class FakeNsdManager(
    private val terminalRemovalTiming: TerminalRemovalTiming =
        TerminalRemovalTiming.AFTER_CALLBACK
) : NsdManager() {
    data class DiscoveryCall(
        val serviceType: String,
        val listener: DiscoveryListener
    )

    data class ResolveCall(
        val info: NsdServiceInfo,
        val listener: ResolveListener
    )

    data class RegistrationCall(
        val info: NsdServiceInfo,
        val listener: RegistrationListener
    )

    val discoveryCalls = mutableListOf<DiscoveryCall>()
    val stoppedDiscoveryListeners = mutableListOf<DiscoveryListener>()
    val resolveCalls = mutableListOf<ResolveCall>()
    val registrationCalls = mutableListOf<RegistrationCall>()
    val unregisteredListeners = mutableListOf<RegistrationListener>()
    private val activeDiscoveryTypes = mutableMapOf<DiscoveryListener, String>()
    private val pendingDiscoveryStops = mutableSetOf<DiscoveryListener>()
    private val activeRegistrationListeners = mutableSetOf<RegistrationListener>()
    private val pendingUnregistrationListeners = mutableSetOf<RegistrationListener>()
    var failNextUnregister: Boolean = false
    var unregisterEntered: CountDownLatch? = null
    var unregisterRelease: CountDownLatch? = null

    override fun discoverServices(
        serviceType: String,
        protocolType: Int,
        listener: DiscoveryListener
    ) {
        discoveryCalls.add(DiscoveryCall(serviceType, listener))
        activeDiscoveryTypes[listener] = serviceType
    }

    override fun stopServiceDiscovery(listener: DiscoveryListener) {
        check(activeDiscoveryTypes.containsKey(listener)) {
            "discovery listener is not active"
        }
        stoppedDiscoveryListeners.add(listener)
        pendingDiscoveryStops.add(listener)
    }

    override fun resolveService(serviceInfo: NsdServiceInfo, listener: ResolveListener) {
        resolveCalls.add(ResolveCall(serviceInfo, listener))
    }

    override fun registerService(
        serviceInfo: NsdServiceInfo,
        protocolType: Int,
        listener: RegistrationListener
    ) {
        registrationCalls.add(RegistrationCall(serviceInfo, listener))
        check(activeRegistrationListeners.add(listener)) {
            "registration listener was reused"
        }
    }

    override fun unregisterService(listener: RegistrationListener) {
        check(activeRegistrationListeners.contains(listener)) {
            "registration listener is not active"
        }
        if (failNextUnregister) {
            failNextUnregister = false
            throw IllegalStateException("injected unregister dispatch failure")
        }
        unregisteredListeners.add(listener)
        pendingUnregistrationListeners.add(listener)
        unregisterEntered?.countDown()
        unregisterRelease?.let { release ->
            check(release.await(5, TimeUnit.SECONDS)) { "unregister barrier timed out" }
        }
    }

    fun registrationFailed(call: RegistrationCall, errorCode: Int) {
        terminalRegistrationCallback(call.listener) {
            call.listener.onRegistrationFailed(call.info, errorCode)
        }
    }

    fun serviceUnregistered(call: RegistrationCall) {
        check(pendingUnregistrationListeners.contains(call.listener)) {
            "service was not pending unregistration"
        }
        terminalRegistrationCallback(call.listener) {
            call.listener.onServiceUnregistered(call.info)
        }
    }

    fun unregistrationFailed(call: RegistrationCall, errorCode: Int) {
        check(pendingUnregistrationListeners.contains(call.listener)) {
            "service was not pending unregistration"
        }
        terminalRegistrationCallback(call.listener) {
            call.listener.onUnregistrationFailed(call.info, errorCode)
        }
    }

    fun startDiscoveryFailed(call: DiscoveryCall, errorCode: Int) {
        terminalDiscoveryCallback(call.listener) {
            call.listener.onStartDiscoveryFailed(call.serviceType, errorCode)
        }
    }

    fun stopDiscoveryFailed(call: DiscoveryCall, errorCode: Int) {
        check(pendingDiscoveryStops.contains(call.listener)) {
            "discovery was not pending stop"
        }
        terminalDiscoveryCallback(call.listener) {
            call.listener.onStopDiscoveryFailed(call.serviceType, errorCode)
        }
        pendingDiscoveryStops.remove(call.listener)
    }

    fun discoveryStopped(call: DiscoveryCall) {
        check(pendingDiscoveryStops.contains(call.listener)) {
            "discovery was not pending stop"
        }
        terminalDiscoveryCallback(call.listener) {
            call.listener.onDiscoveryStopped(call.serviceType)
        }
        pendingDiscoveryStops.remove(call.listener)
    }

    private inline fun terminalDiscoveryCallback(
        listener: DiscoveryListener,
        callback: () -> Unit
    ) {
        if (terminalRemovalTiming == TerminalRemovalTiming.BEFORE_CALLBACK) {
            check(activeDiscoveryTypes.remove(listener) != null) {
                "discovery listener is not active"
            }
        }
        callback()
        if (terminalRemovalTiming == TerminalRemovalTiming.AFTER_CALLBACK) {
            check(activeDiscoveryTypes.remove(listener) != null) {
                "discovery listener is not active"
            }
        }
    }

    private inline fun terminalRegistrationCallback(
        listener: RegistrationListener,
        callback: () -> Unit
    ) {
        if (terminalRemovalTiming == TerminalRemovalTiming.BEFORE_CALLBACK) {
            check(activeRegistrationListeners.remove(listener)) {
                "registration listener is not active"
            }
        }
        callback()
        if (terminalRemovalTiming == TerminalRemovalTiming.AFTER_CALLBACK) {
            check(activeRegistrationListeners.remove(listener)) {
                "registration listener is not active"
            }
        }
        pendingUnregistrationListeners.remove(listener)
    }

    fun isRegistrationListenerActive(listener: RegistrationListener): Boolean =
        activeRegistrationListeners.contains(listener)

    fun isUnregistrationPending(listener: RegistrationListener): Boolean =
        pendingUnregistrationListeners.contains(listener)
}

private fun newPlugin(
    terminalRemovalTiming: TerminalRemovalTiming = TerminalRemovalTiming.AFTER_CALLBACK
): Pair<IrohHttpPlugin, FakeNsdManager> {
    val manager = FakeNsdManager(terminalRemovalTiming)
    val activity = Activity()
    activity.setSystemService(Context.NSD_SERVICE, manager)
    return Pair(IrohHttpPlugin(activity), manager)
}

private fun checkThat(condition: Boolean, message: String) {
    if (!condition) throw AssertionError(message)
}

private fun checkEquals(expected: Any?, actual: Any?, message: String) {
    if (expected != actual) {
        throw AssertionError("$message: expected=$expected actual=$actual")
    }
}

private fun checkFails(message: String, block: () -> Unit) {
    try {
        block()
    } catch (_: Throwable) {
        return
    }
    throw AssertionError(message)
}

private fun peerBrowseArgs() = BrowseStartArgs().apply { serviceName = "iroh" }
private fun genericBrowseArgs() = DnsSdBrowseStartArgs().apply { serviceName = "demo" }
private fun pollArgs(id: Long) = BrowsePollArgs().apply { browseId = id }
private fun stopArgs(id: Long) = BrowseStopArgs().apply { browseId = id }

private fun startPeerBrowse(
    plugin: IrohHttpPlugin,
    manager: FakeNsdManager
): Pair<Long, NsdManager.DiscoveryListener> {
    val invoke = Invoke(peerBrowseArgs())
    val callIndex = manager.discoveryCalls.size
    plugin.browse_peers_start(invoke)
    checkEquals(0, invoke.completionCount, "peer browse must wait for readiness")
    val listener = manager.discoveryCalls[callIndex].listener
    listener.onDiscoveryStarted("_iroh._udp")
    checkEquals(1, invoke.completionCount, "peer browse readiness completes exactly once")
    val id = invoke.resolutions.single()!!["browseId"] as Long
    return Pair(id, listener)
}

private fun startGenericBrowse(
    plugin: IrohHttpPlugin,
    manager: FakeNsdManager
): Pair<Long, NsdManager.DiscoveryListener> {
    val invoke = Invoke(genericBrowseArgs())
    val callIndex = manager.discoveryCalls.size
    plugin.browse_start(invoke)
    checkEquals(0, invoke.completionCount, "generic browse must wait for readiness")
    val listener = manager.discoveryCalls[callIndex].listener
    listener.onDiscoveryStarted("_demo._udp")
    val id = invoke.resolutions.single()!!["browseId"] as Long
    return Pair(id, listener)
}

private fun resolvedPeer(instance: String, address: String): NsdServiceInfo =
    NsdServiceInfo().apply {
        serviceName = instance
        host = InetAddress.getByName("192.168.50.9")
        setPort(4555)
        setAttribute("pk", "a".repeat(52))
        setAttribute("address", address)
        setAttribute("relay", "https://relay.example")
    }

private fun testBrowseReadinessAndTerminalConsumption() {
    val (plugin, manager) = newPlugin()

    val failed = Invoke(peerBrowseArgs())
    plugin.browse_peers_start(failed)
    val failedCall = manager.discoveryCalls.last()
    val failedListener = manager.discoveryCalls.last().listener
    manager.startDiscoveryFailed(failedCall, 7)
    failedListener.onStartDiscoveryFailed("_iroh._udp", 7)
    failedListener.onDiscoveryStarted("_iroh._udp")
    checkEquals(1, failed.completionCount, "peer start failure must reject exactly once")
    checkEquals(1, failed.rejections.size, "peer start failure must reject")
    checkThat(
        !manager.stoppedDiscoveryListeners.contains(failedListener),
        "failed start must not stop an AOSP-retired discovery listener"
    )

    val (id, listener) = startPeerBrowse(plugin, manager)
    manager.stopServiceDiscovery(listener)
    manager.stopDiscoveryFailed(manager.discoveryCalls.last(), 9)
    val firstPoll = Invoke(pollArgs(id))
    plugin.browse_peers_poll(firstPoll)
    checkEquals("failed", firstPoll.resolutions.single()!!["status"], "failed state visible")
    checkThat(firstPoll.resolutions.single()!!["error"] != null, "failed state includes error")
    val secondPoll = Invoke(pollArgs(id))
    plugin.browse_peers_poll(secondPoll)
    checkEquals("closed", secondPoll.resolutions.single()!!["status"], "failure consumed once")

    val missing = Invoke(pollArgs(9999))
    plugin.browse_peers_poll(missing)
    checkEquals("closed", missing.resolutions.single()!!["status"], "missing peer handle is closed")

    val genericFailed = Invoke(genericBrowseArgs())
    plugin.browse_start(genericFailed)
    val genericFailedCall = manager.discoveryCalls.last()
    val genericListener = manager.discoveryCalls.last().listener
    manager.startDiscoveryFailed(genericFailedCall, 11)
    genericListener.onStartDiscoveryFailed("_demo._udp", 11)
    checkEquals(1, genericFailed.completionCount, "generic start failure must reject exactly once")

    val (genericId, activeGenericListener) = startGenericBrowse(plugin, manager)
    manager.stopServiceDiscovery(activeGenericListener)
    manager.stopDiscoveryFailed(manager.discoveryCalls.last(), 12)
    val genericTerminal = Invoke(pollArgs(genericId))
    plugin.browse_poll(genericTerminal)
    checkEquals(
        "failed",
        genericTerminal.resolutions.single()!!["status"],
        "generic failed state visible"
    )
    val genericConsumed = Invoke(pollArgs(genericId))
    plugin.browse_poll(genericConsumed)
    checkEquals(
        "closed",
        genericConsumed.resolutions.single()!!["status"],
        "generic failure consumed once"
    )

    val (closedId, closedListener) = startPeerBrowse(plugin, manager)
    manager.stopServiceDiscovery(closedListener)
    manager.discoveryStopped(manager.discoveryCalls.last())
    val closedTerminal = Invoke(pollArgs(closedId))
    plugin.browse_peers_poll(closedTerminal)
    checkEquals(
        "closed",
        closedTerminal.resolutions.single()!!["status"],
        "native discovery stop is visible as a terminal state"
    )

    val (stoppedId, stoppedListener) = startPeerBrowse(plugin, manager)
    val stop = Invoke(stopArgs(stoppedId))
    plugin.browse_peers_stop(stop)
    checkEquals(1, stop.completionCount, "explicit peer stop resolves")
    checkThat(
        manager.stoppedDiscoveryListeners.contains(stoppedListener),
        "peer stop executes the native stopServiceDiscovery callback sequence"
    )
    manager.discoveryStopped(manager.discoveryCalls.last())
    val stoppedPoll = Invoke(pollArgs(stoppedId))
    plugin.browse_peers_poll(stoppedPoll)
    checkEquals(
        "closed",
        stoppedPoll.resolutions.single()!!["status"],
        "explicitly stopped peer browse is closed"
    )
}

private fun testPeerPresenceGenerationsAndPluralTxt() {
    val (plugin, manager) = newPlugin()
    val (id, listener) = startPeerBrowse(plugin, manager)

    val pending = NsdServiceInfo().apply { serviceName = "late-instance" }
    listener.onServiceFound(pending)
    val lateResolve = manager.resolveCalls.last().listener
    listener.onServiceLost(pending)
    lateResolve.onServiceResolved(resolvedPeer("late-instance", "192.168.50.2:4433"))
    val latePoll = Invoke(pollArgs(id))
    plugin.browse_peers_poll(latePoll)
    val lateEvents = latePoll.resolutions.single()!!["events"] as JSONArray
    checkEquals(0, lateEvents.length(), "found-lost-late-resolve must not emit")

    // Two found callbacks for one instance supersede the first queued resolve.
    val first = NsdServiceInfo().apply { serviceName = "peer-instance" }
    val second = NsdServiceInfo().apply { serviceName = "peer-instance" }
    listener.onServiceFound(first)
    val firstResolve = manager.resolveCalls.last().listener
    listener.onServiceFound(second)
    firstResolve.onServiceResolved(resolvedPeer("peer-instance", "192.168.50.3:4433"))
    val secondResolve = manager.resolveCalls.last().listener
    secondResolve.onServiceResolved(
        resolvedPeer(
            "peer-instance",
            " 192.168.50.4:4433,broken,[fd00::1]:4434,[fe80::1%7]:4435,"
                + "[fe80::1%wlan0]:4436,192.168.50.4:4433,10.0.0.3:1"
        )
    )

    val poll = Invoke(pollArgs(id))
    plugin.browse_peers_poll(poll)
    val events = poll.resolutions.single()!!["events"] as JSONArray
    checkEquals(1, events.length(), "only current presence generation emits")
    val event = events[0] as JSObject
    checkEquals("peer-instance", event["instanceName"], "peer event includes instance name")
    val addrs = (event["addrs"] as JSONArray).toList()
    checkEquals(
        listOf(
            "192.168.50.4:4433",
            "[fd00::1]:4434",
            "[fe80::1%7]:4435",
            "https://relay.example"
        ),
        addrs,
        "plural TXT keeps valid members and de-duplicates"
    )

    val fallback = NsdServiceInfo().apply { serviceName = "fallback-instance" }
    listener.onServiceFound(fallback)
    manager.resolveCalls.last().listener.onServiceResolved(
        resolvedPeer("fallback-instance", "invalid,10.0.0.1:1")
    )
    val fallbackPoll = Invoke(pollArgs(id))
    plugin.browse_peers_poll(fallbackPoll)
    val fallbackEvent = (fallbackPoll.resolutions.single()!!["events"] as JSONArray)[0] as JSObject
    checkEquals(
        listOf("192.168.50.9:4555", "https://relay.example"),
        (fallbackEvent["addrs"] as JSONArray).toList(),
        "invalid TXT must not suppress SRV or relay fallback"
    )
}

private fun testGenericPresenceGeneration() {
    val (plugin, manager) = newPlugin()
    val (id, listener) = startGenericBrowse(plugin, manager)
    val pending = NsdServiceInfo().apply { serviceName = "late-generic" }
    listener.onServiceFound(pending)
    val resolve = manager.resolveCalls.last().listener
    listener.onServiceLost(pending)
    resolve.onServiceResolved(
        NsdServiceInfo().apply {
            serviceName = "late-generic"
            host = InetAddress.getByName("192.168.1.8")
            setPort(8080)
        }
    )
    val poll = Invoke(pollArgs(id))
    plugin.browse_poll(poll)
    checkEquals(
        0,
        (poll.resolutions.single()!!["records"] as JSONArray).length(),
        "generic found-lost-late-resolve must not emit"
    )
}

private fun testPeerInstanceIdentityAndLateHandleCallbacks() {
    val (plugin, manager) = newPlugin()
    val (id, listener) = startPeerBrowse(plugin, manager)
    val originalCall = manager.discoveryCalls.last()

    // Two simultaneously-present DNS-SD instances may intentionally carry the
    // same stable node id. Native events must retain the instance identity so
    // expiry of A cannot retract B's source contribution in Rust.
    val instanceA = NsdServiceInfo().apply { serviceName = "peer-a" }
    listener.onServiceFound(instanceA)
    manager.resolveCalls.last().listener.onServiceResolved(
        resolvedPeer("peer-a", "192.168.10.2:4433")
    )
    val instanceB = NsdServiceInfo().apply { serviceName = "peer-b" }
    listener.onServiceFound(instanceB)
    manager.resolveCalls.last().listener.onServiceResolved(
        resolvedPeer("peer-b", "192.168.10.3:4433")
    )

    val discovered = Invoke(pollArgs(id))
    plugin.browse_peers_poll(discovered)
    val discoveryEvents = discovered.resolutions.single()!!["events"] as JSONArray
    checkEquals(2, discoveryEvents.length(), "both same-node sources are emitted")
    checkEquals(
        listOf("peer-a", "peer-b"),
        discoveryEvents.toList().map { (it as JSObject)["instanceName"] },
        "each discovery carries its own source instance"
    )

    listener.onServiceLost(instanceA)
    val expiredA = Invoke(pollArgs(id))
    plugin.browse_peers_poll(expiredA)
    val expiryEvents = expiredA.resolutions.single()!!["events"] as JSONArray
    checkEquals(1, expiryEvents.length(), "only the lost instance expires")
    checkEquals(
        "peer-a",
        (expiryEvents[0] as JSObject)["instanceName"],
        "expiry retains the exact source identity"
    )

    // A stable duplicate for B remains suppressed, proving A's loss did not
    // accidentally delete B's native snapshot.
    listener.onServiceFound(instanceB)
    manager.resolveCalls.last().listener.onServiceResolved(
        resolvedPeer("peer-b", "192.168.10.3:4433")
    )
    val stableB = Invoke(pollArgs(id))
    plugin.browse_peers_poll(stableB)
    checkEquals(
        0,
        (stableB.resolutions.single()!!["events"] as JSONArray).length(),
        "the still-live source retains its snapshot after another source expires"
    )

    plugin.browse_peers_stop(Invoke(stopArgs(id)))
    val resolvesBeforeLateCallback = manager.resolveCalls.size
    val (replacementId, _) = startPeerBrowse(plugin, manager)
    checkThat(replacementId != id, "browse handles are monotonic and never ABA-reused")
    listener.onServiceFound(NsdServiceInfo().apply { serviceName = "late-old-session" })
    checkEquals(
        resolvesBeforeLateCallback,
        manager.resolveCalls.size,
        "late callbacks from a retired handle cannot enqueue into its replacement"
    )
    manager.discoveryStopped(originalCall)
    plugin.browse_peers_stop(Invoke(stopArgs(replacementId)))
    manager.discoveryStopped(manager.discoveryCalls.last())
}

private fun testRetiredResolveQueuesDoNotStarveNewSessions() {
    val (plugin, manager) = newPlugin()

    // Peer request A1 is in flight; A2 and generic B1 are queued behind it.
    // Closing peer A must purge A2, so completing A1 advances directly to B1.
    val (peerId, peerListener) = startPeerBrowse(plugin, manager)
    val peerA1 = NsdServiceInfo().apply { serviceName = "peer-a1" }
    val peerA2 = NsdServiceInfo().apply { serviceName = "peer-a2-stale" }
    peerListener.onServiceFound(peerA1)
    val peerA1Resolve = manager.resolveCalls.single()
    peerListener.onServiceFound(peerA2)
    val (genericId, genericListener) = startGenericBrowse(plugin, manager)
    val genericB1 = NsdServiceInfo().apply { serviceName = "generic-b1" }
    genericListener.onServiceFound(genericB1)
    checkEquals(1, manager.resolveCalls.size, "legacy resolver keeps one request in flight")

    plugin.browse_peers_stop(Invoke(stopArgs(peerId)))
    peerA1Resolve.listener.onResolveFailed(peerA1, 21)
    checkEquals(2, manager.resolveCalls.size, "new generic session advances after peer retirement")
    checkThat(
        manager.resolveCalls.last().info === genericB1,
        "retired peer queue entry is skipped instead of starving generic browse"
    )
    manager.resolveCalls.last().listener.onResolveFailed(genericB1, 22)

    // Reverse the ownership: a generic session has active/queued work and a
    // new peer waits behind it. Retiring generic B must purge only B's queue.
    val genericB2 = NsdServiceInfo().apply { serviceName = "generic-b2" }
    val genericB3 = NsdServiceInfo().apply { serviceName = "generic-b3-stale" }
    genericListener.onServiceFound(genericB2)
    val genericB2Resolve = manager.resolveCalls.last()
    genericListener.onServiceFound(genericB3)
    val (peerCId, peerCListener) = startPeerBrowse(plugin, manager)
    val peerC1 = NsdServiceInfo().apply { serviceName = "peer-c1" }
    peerCListener.onServiceFound(peerC1)

    plugin.browse_stop(Invoke(stopArgs(genericId)))
    genericB2Resolve.listener.onResolveFailed(genericB2, 23)
    checkThat(
        manager.resolveCalls.last().info === peerC1,
        "retired generic queue entry is skipped instead of starving peer browse"
    )
    manager.resolveCalls.last().listener.onResolveFailed(peerC1, 24)
    plugin.browse_peers_stop(Invoke(stopArgs(peerCId)))
}

private fun peerAdvertiseArgs() = AdvertiseStartArgs().apply {
    serviceName = "iroh"
    pk = "b".repeat(52)
    relay = "https://relay.example"
    addresses = listOf(
        "192.168.1.2:4433",
        "invalid",
        "[fd00::2]:4434",
        "[fe80::2%9]:4435",
        "[fe80::2%wlan0]:4436",
        "192.168.1.2:4433"
    )
}

private fun advertiseStopArgs(id: Long) = AdvertiseStopArgs().apply { advertiseId = id }

private fun testAddressTxtByteBoundary() {
    // The TXT wire entry is `address=<value>`: seven key bytes plus `=` leave
    // exactly 247 UTF-8 bytes for the value.
    val rawInfo = NsdServiceInfo()
    val exactly247Bytes = "é".repeat(123) + "a"
    checkEquals(
        247,
        exactly247Bytes.toByteArray(StandardCharsets.UTF_8).size,
        "boundary fixture is exactly 247 UTF-8 bytes"
    )
    rawInfo.setAttribute("address", exactly247Bytes)
    val exactly248Bytes = "é".repeat(124)
    checkEquals(
        248,
        exactly248Bytes.toByteArray(StandardCharsets.UTF_8).size,
        "overflow fixture is exactly 248 UTF-8 bytes"
    )
    checkFails("stub must reject a 248-byte address TXT value") {
        rawInfo.setAttribute("address", exactly248Bytes)
    }

    // This ordered set of valid socket literals is exactly 247 bytes. The
    // following candidate must be omitted whole rather than truncated.
    val exactPrefix = (1..17).map { index ->
        val port = if (index <= 2) 20000 else 2000
        "10.0.0.$index:$port"
    }
    val expected = exactPrefix.joinToString(",")
    checkEquals(
        247,
        expected.toByteArray(StandardCharsets.UTF_8).size,
        "valid socket set is exactly 247 bytes"
    )

    val (plugin, manager) = newPlugin()
    val invoke = Invoke(AdvertiseStartArgs().apply {
        serviceName = "iroh"
        pk = "c".repeat(52)
        addresses = exactPrefix + listOf("10.0.0.18:2000")
    })
    plugin.advertise_peer_start(invoke)
    val registration = manager.registrationCalls.single()
    val actual = registration.info.attributes["address"]
        ?.let { String(it, StandardCharsets.UTF_8) }
    checkEquals(expected, actual, "advertisement keeps the exact fitting subset")
    checkEquals(
        247,
        actual!!.toByteArray(StandardCharsets.UTF_8).size,
        "advertised address TXT is capped at 247 bytes"
    )
    checkThat(!actual.contains("10.0.0.18:2000"), "overflow candidate is omitted whole")
    registration.listener.onServiceRegistered(registration.info)
    val id = invoke.resolutions.single()!!["advertiseId"] as Long
    plugin.advertise_peer_stop(Invoke(advertiseStopArgs(id)))

    // Matching desktop policy, a long member that does not fit is skipped and
    // a later shorter member may still use the remaining budget.
    val fittingBase = exactPrefix.take(16)
    val longCandidate = "[2001:db8:1234:5678:9abc:def0:1234:5678]:4433"
    val laterShortCandidate = "8.8.8.8:2"
    val subsetInvoke = Invoke(AdvertiseStartArgs().apply {
        serviceName = "iroh"
        pk = "d".repeat(52)
        addresses = fittingBase + listOf(longCandidate, laterShortCandidate)
    })
    plugin.advertise_peer_start(subsetInvoke)
    val subsetRegistration = manager.registrationCalls.last()
    val subset = subsetRegistration.info.attributes["address"]
        ?.let { String(it, StandardCharsets.UTF_8) }!!
    checkThat(!subset.contains(longCandidate), "non-fitting long candidate is skipped")
    checkThat(
        subset.endsWith(laterShortCandidate),
        "later shorter candidate uses the remaining TXT budget"
    )
    checkThat(
        subset.toByteArray(StandardCharsets.UTF_8).size <= 247,
        "stable fitting subset stays within the value budget"
    )
    subsetRegistration.listener.onServiceRegistered(subsetRegistration.info)
    val subsetId = subsetInvoke.resolutions.single()!!["advertiseId"] as Long
    plugin.advertise_peer_stop(Invoke(advertiseStopArgs(subsetId)))
}

private fun startPeerAdvertisement(
    plugin: IrohHttpPlugin,
    manager: FakeNsdManager
): Pair<Long, FakeNsdManager.RegistrationCall> {
    val invoke = Invoke(peerAdvertiseArgs())
    val callIndex = manager.registrationCalls.size
    plugin.advertise_peer_start(invoke)
    checkEquals(0, invoke.completionCount, "peer advertise waits for registration ack")
    val call = manager.registrationCalls[callIndex]
    val address = call.info.attributes["address"]?.let { String(it, StandardCharsets.UTF_8) }
    checkEquals(
        "192.168.1.2:4433,[fd00::2]:4434,[fe80::2%9]:4435",
        address,
        "peer advertisement publishes one plural address TXT"
    )
    call.listener.onServiceRegistered(call.info)
    val id = invoke.resolutions.single()!!["advertiseId"] as Long
    return Pair(id, call)
}

private fun testAdvertisementAckUpdateAndRaces() {
    val (plugin, manager) = newPlugin()

    val failed = Invoke(peerAdvertiseArgs())
    plugin.advertise_peer_start(failed)
    val failedCall = manager.registrationCalls.last()
    manager.registrationFailed(failedCall, 3)
    failedCall.listener.onRegistrationFailed(failedCall.info, 3)
    failedCall.listener.onServiceRegistered(failedCall.info)
    checkEquals(1, failed.completionCount, "registration failure completes start exactly once")

    val (id, initial) = startPeerAdvertisement(plugin, manager)
    val updateArgs = AdvertiseUpdateArgs().apply {
        advertiseId = id
        relay = "https://new-relay.example"
        addresses = listOf("10.0.0.2:5000", "bad", "10.0.0.2:5000")
    }
    val update = Invoke(updateArgs)
    plugin.advertise_peer_update(update)
    checkEquals(0, update.completionCount, "update waits for unregister/register callbacks")
    checkThat(
        manager.unregisteredListeners.last() === initial.listener,
        "update unregisters the current listener"
    )
    manager.serviceUnregistered(initial)
    val replacement = manager.registrationCalls.last()
    checkEquals(
        "b".repeat(52),
        replacement.info.attributes["pk"]?.let { String(it, StandardCharsets.UTF_8) },
        "update preserves pk"
    )
    checkEquals(0, update.completionCount, "update waits for replacement registration ack")
    replacement.listener.onServiceRegistered(replacement.info)
    checkEquals(1, update.completionCount, "replacement ack resolves update once")

    // Stop while the next update is waiting for unregistration. The late
    // callback must not re-register, and the update rejects exactly once.
    val racingUpdate = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = id
        addresses = listOf("10.0.0.3:5001")
    })
    plugin.advertise_peer_update(racingUpdate)
    val registrationsBeforeStop = manager.registrationCalls.size
    val stop = Invoke(advertiseStopArgs(id))
    plugin.advertise_peer_stop(stop)
    checkEquals(1, racingUpdate.rejections.size, "stop rejects in-flight update exactly once")
    checkEquals(1, stop.completionCount, "stop resolves")

    val cleanupCallsBeforeFailure = manager.unregisteredListeners.size
    manager.unregistrationFailed(replacement, 17)
    checkEquals(
        cleanupCallsBeforeFailure,
        manager.unregisteredListeners.size,
        "failed ownerless unregister is never retried"
    )
    replacement.listener.onUnregistrationFailed(replacement.info, 17)
    checkEquals(
        cleanupCallsBeforeFailure,
        manager.unregisteredListeners.size,
        "invalidated ownerless listener is never reused"
    )
    replacement.listener.onServiceUnregistered(replacement.info)
    checkEquals(registrationsBeforeStop, manager.registrationCalls.size, "late unregister cannot revive")
    val stopAgain = Invoke(advertiseStopArgs(id))
    plugin.advertise_peer_stop(stopAgain)
    checkEquals(1, stopAgain.completionCount, "stop is idempotent")

    // Also cover stop after the replacement registration has been launched but
    // before Android acknowledges it.
    val (raceId, raceInitial) = startPeerAdvertisement(plugin, manager)
    val registerRace = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = raceId
        addresses = listOf("10.0.0.4:5002")
    })
    plugin.advertise_peer_update(registerRace)
    manager.serviceUnregistered(raceInitial)
    val pendingReplacement = manager.registrationCalls.last()
    plugin.advertise_peer_stop(Invoke(advertiseStopArgs(raceId)))
    pendingReplacement.listener.onServiceRegistered(pendingReplacement.info)
    checkEquals(1, registerRace.rejections.size, "stop/register race rejects update once")
    checkEquals(0, registerRace.resolutions.size, "stopped update never resolves")
    checkThat(
        manager.unregisteredListeners.count { it === pendingReplacement.listener } >= 1,
        "late registration is explicitly torn down"
    )
    manager.serviceUnregistered(pendingReplacement)

    // AOSP invalidates the listener mapping before delivering an unregister
    // failure. The advertisement must become terminal: it cannot truthfully
    // return to ACTIVE, update again, or try to stop with the dead listener.
    val (failureId, failureInitial) = startPeerAdvertisement(plugin, manager)
    val failedUpdate = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = failureId
        addresses = listOf("10.0.0.5:5003")
    })
    plugin.advertise_peer_update(failedUpdate)
    checkThat(
        manager.isRegistrationListenerActive(failureInitial.listener) &&
            manager.isUnregistrationPending(failureInitial.listener),
        "API 21 keeps the listener mapped until the unregister callback boundary"
    )
    val unregisterCallsAtFailure = manager.unregisteredListeners.size
    manager.unregistrationFailed(failureInitial, 19)
    checkEquals(1, failedUpdate.rejections.size, "unregister failure rejects update")

    val subsequentUpdate = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = failureId
        addresses = listOf("10.0.0.6:5004")
    })
    plugin.advertise_peer_update(subsequentUpdate)
    checkEquals(1, subsequentUpdate.rejections.size, "terminal advertisement rejects updates")
    val terminalStop = Invoke(advertiseStopArgs(failureId))
    plugin.advertise_peer_stop(terminalStop)
    checkEquals(1, terminalStop.completionCount, "terminal advertisement still stops cleanly")
    checkEquals(
        unregisterCallsAtFailure,
        manager.unregisteredListeners.size,
        "update/stop never reuse the invalidated listener"
    )
}

private fun testGenericAdvertiseContract() {
    val (plugin, manager) = newPlugin()
    val rejectedArgs = DnsSdAdvertiseStartArgs().apply {
        serviceName = "demo"
        instanceName = "instance"
        port = 8080
        addrs = listOf("192.168.1.5:8080")
    }
    val rejected = Invoke(rejectedArgs)
    plugin.advertise_start(rejected)
    checkEquals(1, rejected.rejections.size, "generic explicit addrs are rejected")
    checkEquals(0, manager.registrationCalls.size, "rejected generic addrs never register")

    val zeroPort = Invoke(DnsSdAdvertiseStartArgs().apply {
        serviceName = "demo"
        instanceName = "instance"
        port = 0
    })
    plugin.advertise_start(zeroPort)
    checkEquals(1, zeroPort.rejections.size, "generic port zero is rejected")

    val acceptedArgs = DnsSdAdvertiseStartArgs().apply {
        serviceName = "demo"
        instanceName = "instance"
        port = 8080
        txt = mapOf("k" to "v")
    }
    val accepted = Invoke(acceptedArgs)
    plugin.advertise_start(accepted)
    checkEquals(0, accepted.completionCount, "generic advertise waits for registration ack")
    val registration = manager.registrationCalls.single()
    registration.listener.onServiceRegistered(registration.info)
    val id = accepted.resolutions.single()!!["advertiseId"] as Long
    val stop = Invoke(advertiseStopArgs(id))
    plugin.advertise_stop(stop)
    plugin.advertise_stop(Invoke(advertiseStopArgs(id)))
    checkEquals(1, stop.completionCount, "generic stop resolves")
}

private fun testThreadedUpdateStopRace() {
    val (plugin, manager) = newPlugin()
    val (id, _) = startPeerAdvertisement(plugin, manager)
    val unregisterEntered = CountDownLatch(1)
    val unregisterRelease = CountDownLatch(1)
    manager.unregisterEntered = unregisterEntered
    manager.unregisterRelease = unregisterRelease

    val update = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = id
        addresses = listOf("10.0.0.7:5005")
    })
    val stop = Invoke(advertiseStopArgs(id))
    val failure = AtomicReference<Throwable?>(null)
    val updateThread = Thread {
        try {
            plugin.advertise_peer_update(update)
        } catch (error: Throwable) {
            failure.compareAndSet(null, error)
        }
    }
    updateThread.start()
    checkThat(
        unregisterEntered.await(5, TimeUnit.SECONDS),
        "update reached the native unregister barrier"
    )

    val stopStarted = CountDownLatch(1)
    val stopThread = Thread {
        stopStarted.countDown()
        try {
            plugin.advertise_peer_stop(stop)
        } catch (error: Throwable) {
            failure.compareAndSet(null, error)
        }
    }
    stopThread.start()
    checkThat(stopStarted.await(5, TimeUnit.SECONDS), "stop thread started")
    unregisterRelease.countDown()
    updateThread.join(5_000)
    stopThread.join(5_000)
    manager.unregisterEntered = null
    manager.unregisterRelease = null

    checkThat(!updateThread.isAlive && !stopThread.isAlive, "update/stop race cannot deadlock")
    failure.get()?.let { throw AssertionError("threaded update/stop race failed", it) }
    checkEquals(1, update.rejections.size, "stop rejects the racing update exactly once")
    checkEquals(0, update.resolutions.size, "racing update never resolves")
    checkEquals(1, stop.completionCount, "racing stop resolves exactly once")
}

private fun testAospListenerLifecycleAcrossApiEras() {
    for (timing in TerminalRemovalTiming.values()) {
        val (plugin, manager) = newPlugin(timing)

        val failedBrowse = Invoke(peerBrowseArgs())
        plugin.browse_peers_start(failedBrowse)
        val failedBrowseCall = manager.discoveryCalls.last()
        manager.startDiscoveryFailed(failedBrowseCall, 31)
        checkEquals(1, failedBrowse.rejections.size, "$timing browse failure rejects once")
        checkThat(
            !manager.stoppedDiscoveryListeners.contains(failedBrowseCall.listener),
            "$timing start failure never stops a terminal listener"
        )

        val (id, initial) = startPeerAdvertisement(plugin, manager)
        val update = Invoke(AdvertiseUpdateArgs().apply {
            advertiseId = id
            addresses = listOf("10.0.0.20:6000")
        })
        plugin.advertise_peer_update(update)
        manager.serviceUnregistered(initial)
        val replacement = manager.registrationCalls.last()
        replacement.listener.onServiceRegistered(replacement.info)
        checkEquals(1, update.resolutions.size, "$timing update resolves once")

        plugin.advertise_peer_stop(Invoke(advertiseStopArgs(id)))
        checkThat(
            manager.isUnregistrationPending(replacement.listener),
            "$timing stop dispatches exactly one unregister"
        )
        manager.serviceUnregistered(replacement)
        checkThat(
            !manager.isRegistrationListenerActive(replacement.listener),
            "$timing terminal callback retires the listener"
        )
    }

    // If an unregister dispatch itself throws, it has not been accepted and
    // must not poison the generation. A late registration acknowledgement can
    // then perform the ownerless cleanup exactly once.
    val (retryPlugin, retryManager) = newPlugin()
    val (retryId, retryInitial) = startPeerAdvertisement(retryPlugin, retryManager)
    val retryUpdate = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = retryId
        addresses = listOf("10.0.0.21:6001")
    })
    retryPlugin.advertise_peer_update(retryUpdate)
    retryManager.serviceUnregistered(retryInitial)
    val pendingReplacement = retryManager.registrationCalls.last()
    val successfulUnregistersBeforeStop = retryManager.unregisteredListeners.size
    retryManager.failNextUnregister = true
    retryPlugin.advertise_peer_stop(Invoke(advertiseStopArgs(retryId)))
    checkEquals(
        successfulUnregistersBeforeStop,
        retryManager.unregisteredListeners.size,
        "failed dispatch is not reported as an issued unregister"
    )
    pendingReplacement.listener.onServiceRegistered(pendingReplacement.info)
    checkEquals(
        successfulUnregistersBeforeStop + 1,
        retryManager.unregisteredListeners.size,
        "late registration retries cleanup after the failed dispatch"
    )
    retryManager.serviceUnregistered(pendingReplacement)
    checkEquals(1, retryUpdate.rejections.size, "stop rejects the pending update once")

    // Conversely, a registration-failure callback is terminal in both AOSP
    // orderings. Stop must not pass its already-retired listener back to NSD.
    val (failurePlugin, failureManager) = newPlugin()
    val (failureId, failureInitial) = startPeerAdvertisement(failurePlugin, failureManager)
    val failedReplacementUpdate = Invoke(AdvertiseUpdateArgs().apply {
        advertiseId = failureId
        addresses = listOf("10.0.0.22:6002")
    })
    failurePlugin.advertise_peer_update(failedReplacementUpdate)
    failureManager.serviceUnregistered(failureInitial)
    val failedReplacement = failureManager.registrationCalls.last()
    failureManager.registrationFailed(failedReplacement, 32)
    val unregistersAtFailure = failureManager.unregisteredListeners.size
    failurePlugin.advertise_peer_stop(Invoke(advertiseStopArgs(failureId)))
    checkEquals(
        unregistersAtFailure,
        failureManager.unregisteredListeners.size,
        "registration-failed listener is never reused during stop"
    )
    checkEquals(
        1,
        failedReplacementUpdate.rejections.size,
        "replacement registration failure rejects update once"
    )
}

fun main() {
    testBrowseReadinessAndTerminalConsumption()
    testPeerPresenceGenerationsAndPluralTxt()
    testGenericPresenceGeneration()
    testPeerInstanceIdentityAndLateHandleCallbacks()
    testRetiredResolveQueuesDoNotStarveNewSessions()
    testAddressTxtByteBoundary()
    testAdvertisementAckUpdateAndRaces()
    testGenericAdvertiseContract()
    testThreadedUpdateStopRace()
    testAospListenerLifecycleAcrossApiEras()
    println("Android native discovery contract: 10/10 test groups passed")
}
