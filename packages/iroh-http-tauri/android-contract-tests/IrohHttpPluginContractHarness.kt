package com.iroh.http

import android.app.Activity
import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import org.json.JSONArray
import org.json.JSONObject
import java.net.InetAddress
import java.nio.charset.StandardCharsets
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ConcurrentHashMap
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
    private val pendingDiscoveryStops = ConcurrentHashMap.newKeySet<DiscoveryListener>()
    private val activeRegistrationListeners = mutableSetOf<RegistrationListener>()
    private val pendingUnregistrationListeners =
        ConcurrentHashMap.newKeySet<RegistrationListener>()
    var failNextUnregister: Boolean = false
    var registerDispatchFailuresRemaining: Int = 0
    var unregisterDispatchFailuresRemaining: Int = 0
    var discoverDispatchFailuresRemaining: Int = 0
    var stopDiscoveryDispatchFailuresRemaining: Int = 0
    var unregisterEntered: CountDownLatch? = null
    var unregisterRelease: CountDownLatch? = null

    override fun discoverServices(
        serviceType: String,
        protocolType: Int,
        listener: DiscoveryListener
    ) {
        discoveryCalls.add(DiscoveryCall(serviceType, listener))
        activeDiscoveryTypes[listener] = serviceType
        if (discoverDispatchFailuresRemaining > 0) {
            discoverDispatchFailuresRemaining -= 1
            throw IllegalStateException("injected discovery dispatch failure")
        }
    }

    override fun stopServiceDiscovery(listener: DiscoveryListener) {
        check(activeDiscoveryTypes.containsKey(listener)) {
            "discovery listener is not active"
        }
        if (stopDiscoveryDispatchFailuresRemaining > 0) {
            stopDiscoveryDispatchFailuresRemaining -= 1
            throw IllegalStateException("injected discovery stop dispatch failure")
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
        if (registerDispatchFailuresRemaining > 0) {
            registerDispatchFailuresRemaining -= 1
            throw IllegalStateException("injected register dispatch failure")
        }
    }

    override fun unregisterService(listener: RegistrationListener) {
        check(activeRegistrationListeners.contains(listener)) {
            "registration listener is not active"
        }
        if (unregisterDispatchFailuresRemaining > 0) {
            unregisterDispatchFailuresRemaining -= 1
            throw IllegalStateException("injected unregister dispatch failure")
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

    fun isDiscoveryListenerActive(listener: DiscoveryListener): Boolean =
        activeDiscoveryTypes.containsKey(listener)

    fun isDiscoveryStopPending(listener: DiscoveryListener): Boolean =
        pendingDiscoveryStops.contains(listener)
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

private fun waitUntil(
    message: String,
    timeoutMillis: Long = 2_000,
    condition: () -> Boolean
) {
    val deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMillis)
    while (!condition() && System.nanoTime() < deadline) {
        Thread.sleep(5)
    }
    checkThat(condition(), message)
}

private fun genericBrowseArgs() = DnsSdBrowseStartArgs().apply { serviceName = "demo" }
private fun pollArgs(id: Long) = BrowsePollArgs().apply { browseId = id }
private fun stopArgs(id: Long) = BrowseStopArgs().apply { browseId = id }

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
    checkEquals(1, invoke.completionCount, "generic browse readiness completes exactly once")
    val id = invoke.resolutions.single()!!["browseId"] as Long
    return Pair(id, listener)
}

private fun testBrowseReadinessAndTerminalConsumption() {
    val (plugin, manager) = newPlugin()

    val (ackPlugin, ackManager) = newPlugin()
    ackManager.discoverDispatchFailuresRemaining = 1
    val acknowledgedStart = Invoke(genericBrowseArgs())
    ackPlugin.browse_start(acknowledgedStart)
    val acknowledgedCall = ackManager.discoveryCalls.single()
    checkEquals(
        0,
        acknowledgedStart.completionCount,
        "ambiguous discovery failure waits for accepted stop acknowledgement"
    )
    checkThat(
        ackManager.isDiscoveryStopPending(acknowledgedCall.listener),
        "ambiguous failed browse dispatches native cleanup"
    )
    ackManager.discoveryStopped(acknowledgedCall)
    checkEquals(1, acknowledgedStart.rejections.size, "failed browse rejects after cleanup terminal")

    val (retainedStartPlugin, retainedStartManager) = newPlugin()
    retainedStartManager.discoverDispatchFailuresRemaining = 1
    retainedStartManager.stopDiscoveryDispatchFailuresRemaining = 2
    val retainedStart = Invoke(genericBrowseArgs())
    retainedStartPlugin.browse_start(retainedStart)
    val retainedStartCall = retainedStartManager.discoveryCalls.single()
    checkEquals(
        0,
        retainedStart.completionCount,
        "exhausted failed-start cleanup keeps the Rust start lease fenced"
    )
    checkThat(
        retainedStartManager.isDiscoveryListenerActive(retainedStartCall.listener),
        "failed browse start retains owner after unaccepted cleanup"
    )
    retainedStartManager.stopDiscoveryDispatchFailuresRemaining = 1
    retainedStartCall.listener.onDiscoveryStarted("_demo._udp")
    checkThat(
        retainedStartManager.isDiscoveryStopPending(retainedStartCall.listener),
        "late browse readiness retries retained failed-start cleanup"
    )
    retainedStartManager.discoveryStopped(retainedStartCall)
    checkEquals(1, retainedStart.rejections.size, "retained browse start rejects after terminal cleanup")
    checkThat(
        !retainedStartManager.isDiscoveryListenerActive(retainedStartCall.listener),
        "retained failed browse owner releases at terminal callback"
    )

    val failed = Invoke(genericBrowseArgs())
    plugin.browse_start(failed)
    val failedCall = manager.discoveryCalls.last()
    val failedListener = manager.discoveryCalls.last().listener
    manager.startDiscoveryFailed(failedCall, 7)
    failedListener.onStartDiscoveryFailed("_demo._udp", 7)
    failedListener.onDiscoveryStarted("_demo._udp")
    checkEquals(1, failed.completionCount, "generic start failure must reject exactly once")
    checkEquals(1, failed.rejections.size, "generic start failure must reject")
    checkThat(
        !manager.stoppedDiscoveryListeners.contains(failedListener),
        "failed start must not stop an AOSP-retired discovery listener"
    )

    val (id, listener) = startGenericBrowse(plugin, manager)
    manager.stopServiceDiscovery(listener)
    manager.stopDiscoveryFailed(manager.discoveryCalls.last(), 9)
    val firstPoll = Invoke(pollArgs(id))
    plugin.browse_poll(firstPoll)
    checkEquals("failed", firstPoll.resolutions.single()!!["status"], "failed state visible")
    checkThat(firstPoll.resolutions.single()!!["error"] != null, "failed state includes error")
    val secondPoll = Invoke(pollArgs(id))
    plugin.browse_poll(secondPoll)
    checkEquals("closed", secondPoll.resolutions.single()!!["status"], "failure consumed once")

    val missing = Invoke(pollArgs(9999))
    plugin.browse_poll(missing)
    checkEquals("closed", missing.resolutions.single()!!["status"], "missing handle is closed")

    val (closedId, closedListener) = startGenericBrowse(plugin, manager)
    manager.stopServiceDiscovery(closedListener)
    manager.discoveryStopped(manager.discoveryCalls.last())
    val closedTerminal = Invoke(pollArgs(closedId))
    plugin.browse_poll(closedTerminal)
    checkEquals(
        "closed",
        closedTerminal.resolutions.single()!!["status"],
        "native discovery stop is visible as a terminal state"
    )

    val (stoppedId, stoppedListener) = startGenericBrowse(plugin, manager)
    val stop = Invoke(stopArgs(stoppedId))
    val duplicateStop = Invoke(stopArgs(stoppedId))
    plugin.browse_stop(stop)
    plugin.browse_stop(duplicateStop)
    checkEquals(0, stop.completionCount, "generic stop waits for native acknowledgement")
    checkEquals(0, duplicateStop.completionCount, "duplicate generic stop shares the waiter")
    checkThat(
        manager.stoppedDiscoveryListeners.contains(stoppedListener),
        "generic stop executes the native stopServiceDiscovery callback sequence"
    )
    manager.discoveryStopped(manager.discoveryCalls.last())
    checkEquals(1, stop.resolutions.size, "generic stop resolves after native acknowledgement")
    checkEquals(1, duplicateStop.resolutions.size, "duplicate stop resolves after acknowledgement")
    val stoppedPoll = Invoke(pollArgs(stoppedId))
    plugin.browse_poll(stoppedPoll)
    checkEquals(
        "closed",
        stoppedPoll.resolutions.single()!!["status"],
        "explicitly stopped generic browse is closed"
    )

    // A stop issued before readiness can be rejected because Android has not
    // activated the listener yet. Keep the waiter and retry after the late
    // readiness callback instead of resolving or losing the native owner.
    val (latePlugin, lateManager) = newPlugin()
    val lateStart = Invoke(genericBrowseArgs())
    latePlugin.browse_start(lateStart)
    val lateCall = lateManager.discoveryCalls.single()
    lateManager.stopDiscoveryDispatchFailuresRemaining = 1
    val lateStop = Invoke(stopArgs(1))
    latePlugin.browse_stop(lateStop)
    checkEquals(1, lateStart.rejections.size, "stop-before-ready rejects start once")
    checkEquals(0, lateStop.completionCount, "stop-before-ready waits for late readiness")
    lateCall.listener.onDiscoveryStarted("_demo._udp")
    checkThat(
        lateManager.isDiscoveryStopPending(lateCall.listener),
        "late readiness retries the deferred discovery stop"
    )
    checkEquals(0, lateStop.completionCount, "late-ready cleanup waits for terminal callback")
    lateManager.discoveryStopped(lateCall)
    checkEquals(1, lateStop.resolutions.size, "late-ready stop resolves after terminal callback")

    // Confirmed active sessions retry a synchronous dispatch rejection once.
    val (retryPlugin, retryManager) = newPlugin()
    val (retryId, retryListener) = startGenericBrowse(retryPlugin, retryManager)
    retryManager.stopDiscoveryDispatchFailuresRemaining = 1
    val retryStop = Invoke(stopArgs(retryId))
    retryPlugin.browse_stop(retryStop)
    checkEquals(0, retryStop.completionCount, "retried browse stop waits for acknowledgement")
    checkThat(
        retryManager.isDiscoveryStopPending(retryListener),
        "active browse stop retries an unaccepted dispatch"
    )
    retryManager.discoveryStopped(retryManager.discoveryCalls.last())
    checkEquals(1, retryStop.resolutions.size, "retried browse stop resolves after acknowledgement")

    // Exhausted immediate retries retain both the original caller and native
    // ownership. The adapter retries after a delay because Rust issues exactly
    // one stop command for a handle.
    val (retainedPlugin, retainedManager) = newPlugin()
    val (retainedId, retainedListener) = startGenericBrowse(retainedPlugin, retainedManager)
    retainedManager.stopDiscoveryDispatchFailuresRemaining = 5
    val failedDispatchStop = Invoke(stopArgs(retainedId))
    retainedPlugin.browse_stop(failedDispatchStop)
    checkEquals(0, failedDispatchStop.completionCount, "browse stop remains pending after immediate retries")
    checkThat(
        retainedManager.isDiscoveryListenerActive(retainedListener),
        "failed browse stop retains its native listener owner"
    )
    waitUntil("browse stop did not autonomously retry after dispatch recovery") {
        retainedManager.isDiscoveryStopPending(retainedListener)
    }
    checkEquals(0, failedDispatchStop.completionCount, "retried browse stop waits for acknowledgement")
    retainedManager.discoveryStopped(retainedManager.discoveryCalls.last())
    checkEquals(1, failedDispatchStop.resolutions.size, "original browse stop closes terminally")

    val (callbackPlugin, callbackManager) = newPlugin()
    val (callbackId, _) = startGenericBrowse(callbackPlugin, callbackManager)
    val callbackStop = Invoke(stopArgs(callbackId))
    callbackPlugin.browse_stop(callbackStop)
    callbackManager.stopDiscoveryFailed(callbackManager.discoveryCalls.last(), 51)
    checkEquals(1, callbackStop.rejections.size, "terminal stop failure rejects its waiter")
}

private fun testGenericBrowseRecordsPresenceAndRepeatedUpserts() {
    val (plugin, manager) = newPlugin()
    val (id, listener) = startGenericBrowse(plugin, manager)

    val advertised = NsdServiceInfo().apply { serviceName = "generic-printer" }
    fun resolvedAdvertisement() = NsdServiceInfo().apply {
        serviceName = "generic-printer"
        host = InetAddress.getByName("192.168.1.8")
        setPort(8080)
        setAttribute("pk", "a".repeat(52))
        setAttribute("address", "192.168.1.8:4433")
        setAttribute("relay", "https://relay.example")
        setAttribute("note", "office")
    }

    // Queue the same presence twice before either resolve completes. Android
    // serializes resolveService on older API levels; both announcements still
    // belong to the same presence epoch and must become observable upserts.
    listener.onServiceFound(advertised)
    listener.onServiceFound(advertised)
    checkEquals(1, manager.resolveCalls.size, "generic resolves are serialized")
    manager.resolveCalls.last().listener.onServiceResolved(resolvedAdvertisement())
    checkEquals(2, manager.resolveCalls.size, "second generic resolve starts after first callback")
    manager.resolveCalls.last().listener.onServiceResolved(resolvedAdvertisement())
    val firstPoll = Invoke(pollArgs(id))
    plugin.browse_poll(firstPoll)
    val firstRecords = firstPoll.resolutions.single()!!["records"] as JSONArray
    checkEquals(2, firstRecords.length(), "queued generic announcements emit two active records")
    val active = firstRecords[0] as JSObject
    checkEquals(true, active["isActive"], "generic record is active")
    checkEquals(
        "_demo._udp",
        active["serviceType"],
        "native record uses the shorthand Rust canonicalizes at the adapter seam"
    )
    checkEquals("generic-printer", active["instanceName"], "generic record keeps instance")
    checkEquals("192.168.1.8", active["host"], "generic record keeps resolved host")
    checkEquals(8080, active["port"], "generic record keeps SRV port")
    checkEquals(
        listOf("192.168.1.8:8080"),
        (active["addrs"] as JSONArray).toList(),
        "generic record pairs its resolved host with the SRV port"
    )
    val txt = active["txt"] as JSObject
    checkEquals("a".repeat(52), txt["pk"], "generic record keeps pk TXT")
    checkEquals("192.168.1.8:4433", txt["address"], "generic record keeps address TXT")
    checkEquals("https://relay.example", txt["relay"], "generic record keeps relay TXT")
    checkEquals("office", txt["note"], "generic record keeps arbitrary TXT")

    // A later platform announcement is likewise observable even when its
    // resolved snapshot is identical.
    listener.onServiceFound(advertised)
    manager.resolveCalls.last().listener.onServiceResolved(resolvedAdvertisement())
    val repeatedPoll = Invoke(pollArgs(id))
    plugin.browse_poll(repeatedPoll)
    checkEquals(
        1,
        (repeatedPoll.resolutions.single()!!["records"] as JSONArray).length(),
        "identical generic announcements remain repeated upserts"
    )

    listener.onServiceLost(advertised)
    val removalPoll = Invoke(pollArgs(id))
    plugin.browse_poll(removalPoll)
    val removalRecords = removalPoll.resolutions.single()!!["records"] as JSONArray
    checkEquals(1, removalRecords.length(), "generic loss emits one removal")
    val removal = removalRecords[0] as JSObject
    checkEquals(false, removal["isActive"], "generic removal is inactive")
    checkEquals(
        "_demo._udp",
        removal["serviceType"],
        "native removal uses the shorthand Rust canonicalizes at the adapter seam"
    )
    checkEquals("generic-printer", removal["instanceName"], "generic removal keeps instance")
    checkEquals(0, removal["port"], "generic removal clears the port")
    checkEquals(0, (removal["addrs"] as JSONArray).length(), "generic removal clears addrs")
    checkEquals(JSONObject.NULL, removal["host"], "generic removal clears host")
    val removalTxt = removal["txt"] as JSObject
    for (key in listOf("pk", "address", "relay", "note")) {
        checkEquals(null, removalTxt[key], "generic removal clears $key TXT")
    }

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

private fun testRetiredResolveQueuesDoNotStarveNewSessions() {
    val (plugin, manager) = newPlugin()

    // Generic request A1 is in flight; A2 and session B1 are queued behind it.
    // Closing A must purge A2, so completing A1 advances directly to B1.
    val (sessionAId, sessionAListener) = startGenericBrowse(plugin, manager)
    val requestA1 = NsdServiceInfo().apply { serviceName = "generic-a1" }
    val requestA2 = NsdServiceInfo().apply { serviceName = "generic-a2-stale" }
    sessionAListener.onServiceFound(requestA1)
    val requestA1Resolve = manager.resolveCalls.single()
    sessionAListener.onServiceFound(requestA2)
    val (sessionBId, sessionBListener) = startGenericBrowse(plugin, manager)
    val requestB1 = NsdServiceInfo().apply { serviceName = "generic-b1" }
    sessionBListener.onServiceFound(requestB1)
    checkEquals(1, manager.resolveCalls.size, "legacy resolver keeps one request in flight")

    plugin.browse_stop(Invoke(stopArgs(sessionAId)))
    requestA1Resolve.listener.onResolveFailed(requestA1, 21)
    checkEquals(2, manager.resolveCalls.size, "live generic session advances after retirement")
    checkThat(
        manager.resolveCalls.last().info === requestB1,
        "retired generic queue entry is skipped instead of starving another browse"
    )
    manager.resolveCalls.last().listener.onResolveFailed(requestB1, 22)
    plugin.browse_stop(Invoke(stopArgs(sessionBId)))
}

private fun advertiseStopArgs(id: Long) = AdvertiseStopArgs().apply { advertiseId = id }

private fun genericAdvertiseUpdateArgs(
    id: Long,
    port: Int = 9090,
    txt: Map<String, String> = mapOf("k" to "updated")
) = DnsSdAdvertiseUpdateArgs().apply {
    advertiseId = id
    this.port = port
    this.txt = txt
}

private fun peerShapedTxt(
    address: String = "192.168.1.2:4433,[fd00::2]:4434,[fe80::2%9]:4435",
    relay: String = "https://relay.example"
): Map<String, String> = mapOf(
    "pk" to "b".repeat(52),
    "relay" to relay,
    "address" to address
)

private fun peerShapedAdvertiseArgs() = DnsSdAdvertiseStartArgs().apply {
    serviceName = "iroh"
    instanceName = "b".repeat(52)
    port = 1
    protocol = "udp"
    txt = peerShapedTxt()
}

private fun startGenericAdvertisement(
    plugin: IrohHttpPlugin,
    manager: FakeNsdManager
): Pair<Long, FakeNsdManager.RegistrationCall> {
    val invoke = Invoke(peerShapedAdvertiseArgs())
    val callIndex = manager.registrationCalls.size
    plugin.advertise_start(invoke)
    checkEquals(0, invoke.completionCount, "generic advertise waits for registration ack")
    val call = manager.registrationCalls[callIndex]
    val address = call.info.attributes["address"]?.let { String(it, StandardCharsets.UTF_8) }
    checkEquals(
        "192.168.1.2:4433,[fd00::2]:4434,[fe80::2%9]:4435",
        address,
        "generic advertisement preserves peer-shaped address TXT"
    )
    call.listener.onServiceRegistered(call.info)
    val id = invoke.resolutions.single()!!["advertiseId"] as Long
    return Pair(id, call)
}

private fun testAdvertisementLifecycleAndRaces() {
    val (plugin, manager) = newPlugin()

    val failed = Invoke(peerShapedAdvertiseArgs())
    plugin.advertise_start(failed)
    val failedCall = manager.registrationCalls.last()
    manager.registrationFailed(failedCall, 3)
    failedCall.listener.onRegistrationFailed(failedCall.info, 3)
    failedCall.listener.onServiceRegistered(failedCall.info)
    checkEquals(1, failed.completionCount, "registration failure completes start exactly once")

    // Exhausting the immediate native retries keeps the original caller and
    // owner pending. The adapter retries after a delay because the production
    // Rust actor sends no second stop command.
    val (retainedPlugin, retainedManager) = newPlugin()
    val (retainedId, retainedRegistration) =
        startGenericAdvertisement(retainedPlugin, retainedManager)
    retainedManager.unregisterDispatchFailuresRemaining = 5
    val failedDispatchStop = Invoke(advertiseStopArgs(retainedId))
    retainedPlugin.advertise_stop(failedDispatchStop)
    checkEquals(0, failedDispatchStop.completionCount, "generic stop remains pending after immediate retries")
    checkThat(
        retainedManager.isRegistrationListenerActive(retainedRegistration.listener),
        "failed generic stop retains ownership of the active registration"
    )
    waitUntil("generic stop did not autonomously retry after dispatch recovery") {
        retainedManager.isUnregistrationPending(retainedRegistration.listener)
    }
    checkEquals(0, failedDispatchStop.completionCount, "retried generic stop waits for cleanup")
    retainedManager.serviceUnregistered(retainedRegistration)
    checkEquals(1, failedDispatchStop.resolutions.size, "original generic owner closes after callback")

    val (id, initial) = startGenericAdvertisement(plugin, manager)
    val updateArgs = genericAdvertiseUpdateArgs(
        id,
        port = 1,
        txt = peerShapedTxt("10.0.0.2:5000", "https://new-relay.example")
    )
    val update = Invoke(updateArgs)
    plugin.advertise_update(update)
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
    val racingUpdate = Invoke(
        genericAdvertiseUpdateArgs(id, port = 1, txt = peerShapedTxt("10.0.0.3:5001"))
    )
    plugin.advertise_update(racingUpdate)
    val registrationsBeforeStop = manager.registrationCalls.size
    val stop = Invoke(advertiseStopArgs(id))
    plugin.advertise_stop(stop)
    checkEquals(1, racingUpdate.rejections.size, "stop rejects in-flight update exactly once")
    checkEquals(0, stop.completionCount, "stop waits for the in-flight unregister callback")

    val cleanupCallsBeforeFailure = manager.unregisteredListeners.size
    manager.unregistrationFailed(replacement, 17)
    checkEquals(1, stop.rejections.size, "stop surfaces terminal unregistration failure")
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
    plugin.advertise_stop(stopAgain)
    checkEquals(1, stopAgain.completionCount, "stop is idempotent")

    // Also cover stop after the replacement registration has been launched but
    // before Android acknowledges it.
    val (raceId, raceInitial) = startGenericAdvertisement(plugin, manager)
    val registerRace = Invoke(
        genericAdvertiseUpdateArgs(raceId, port = 1, txt = peerShapedTxt("10.0.0.4:5002"))
    )
    plugin.advertise_update(registerRace)
    manager.serviceUnregistered(raceInitial)
    val pendingReplacement = manager.registrationCalls.last()
    plugin.advertise_stop(Invoke(advertiseStopArgs(raceId)))
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
    val (failureId, failureInitial) = startGenericAdvertisement(plugin, manager)
    val failedUpdate = Invoke(
        genericAdvertiseUpdateArgs(failureId, port = 1, txt = peerShapedTxt("10.0.0.5:5003"))
    )
    plugin.advertise_update(failedUpdate)
    checkThat(
        manager.isRegistrationListenerActive(failureInitial.listener) &&
            manager.isUnregistrationPending(failureInitial.listener),
        "API 21 keeps the listener mapped until the unregister callback boundary"
    )
    val unregisterCallsAtFailure = manager.unregisteredListeners.size
    manager.unregistrationFailed(failureInitial, 19)
    checkEquals(1, failedUpdate.rejections.size, "unregister failure rejects update")

    val subsequentUpdate = Invoke(
        genericAdvertiseUpdateArgs(failureId, port = 1, txt = peerShapedTxt("10.0.0.6:5004"))
    )
    plugin.advertise_update(subsequentUpdate)
    checkEquals(1, subsequentUpdate.rejections.size, "terminal advertisement rejects updates")
    val terminalStop = Invoke(advertiseStopArgs(failureId))
    plugin.advertise_stop(terminalStop)
    checkEquals(1, terminalStop.completionCount, "terminal advertisement still stops cleanly")
    checkEquals(
        unregisterCallsAtFailure,
        manager.unregisteredListeners.size,
        "update/stop never reuse the invalidated listener"
    )
}

private fun testGenericAdvertiseContract() {
    val (plugin, manager) = newPlugin()

    val (ackPlugin, ackManager) = newPlugin()
    ackManager.registerDispatchFailuresRemaining = 1
    val acknowledgedStart = Invoke(peerShapedAdvertiseArgs())
    ackPlugin.advertise_start(acknowledgedStart)
    val acknowledgedRegistration = ackManager.registrationCalls.single()
    checkEquals(
        0,
        acknowledgedStart.completionCount,
        "ambiguous register failure waits for accepted cleanup acknowledgement"
    )
    checkThat(
        ackManager.isUnregistrationPending(acknowledgedRegistration.listener),
        "ambiguous failed start dispatches native cleanup"
    )
    ackManager.serviceUnregistered(acknowledgedRegistration)
    checkEquals(1, acknowledgedStart.rejections.size, "failed start rejects after cleanup terminal")

    // registerService can throw after installing the listener. If cleanup also
    // rejects dispatch, the failed start retains an internal owner so a late
    // readiness callback can retry and await terminal unregistration.
    val (latePlugin, lateManager) = newPlugin()
    lateManager.registerDispatchFailuresRemaining = 1
    lateManager.unregisterDispatchFailuresRemaining = 2
    val lateStart = Invoke(peerShapedAdvertiseArgs())
    latePlugin.advertise_start(lateStart)
    val lateRegistration = lateManager.registrationCalls.single()
    checkEquals(
        0,
        lateStart.completionCount,
        "unaccepted failed-start cleanup keeps the Rust start lease fenced"
    )
    checkThat(
        lateManager.isRegistrationListenerActive(lateRegistration.listener),
        "failed start retains ownership while cleanup is unaccepted"
    )
    lateManager.failNextUnregister = true
    lateRegistration.listener.onServiceRegistered(lateRegistration.info)
    checkThat(
        lateManager.isUnregistrationPending(lateRegistration.listener),
        "late readiness retries cleanup for the retained failed start"
    )
    lateManager.serviceUnregistered(lateRegistration)
    checkEquals(1, lateStart.rejections.size, "retained advertise start rejects after terminal cleanup")
    checkThat(
        !lateManager.isRegistrationListenerActive(lateRegistration.listener),
        "failed start owner is released only at the terminal callback"
    )

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

    val rejectedUpdate = Invoke(genericAdvertiseUpdateArgs(id).apply {
        addrs = listOf("192.168.1.6")
    })
    plugin.advertise_update(rejectedUpdate)
    checkEquals(1, rejectedUpdate.rejections.size, "generic update rejects explicit addrs")
    checkEquals(0, manager.unregisteredListeners.size, "rejected generic update stays registered")

    val update = Invoke(genericAdvertiseUpdateArgs(id))
    plugin.advertise_update(update)
    checkEquals(0, update.completionCount, "generic update waits for unregister callback")
    checkThat(
        manager.unregisteredListeners.last() === registration.listener,
        "generic update unregisters the current listener"
    )
    manager.serviceUnregistered(registration)
    val replacement = manager.registrationCalls.last()
    checkEquals("instance", replacement.info.serviceName, "generic update preserves instance")
    checkEquals("_demo._udp", replacement.info.serviceType, "generic update preserves service type")
    checkEquals(9090, replacement.info.port, "generic update changes port")
    checkEquals(
        "updated",
        replacement.info.attributes["k"]?.let { String(it, StandardCharsets.UTF_8) },
        "generic update changes TXT"
    )
    checkEquals(0, update.completionCount, "generic update waits for replacement readiness")
    replacement.listener.onServiceRegistered(replacement.info)
    checkEquals(1, update.resolutions.size, "generic replacement readiness resolves update")

    val stop = Invoke(advertiseStopArgs(id))
    val duplicateStop = Invoke(advertiseStopArgs(id))
    plugin.advertise_stop(stop)
    plugin.advertise_stop(duplicateStop)
    checkEquals(0, stop.completionCount, "generic stop waits for native unregistration")
    checkEquals(0, duplicateStop.completionCount, "duplicate stop shares the cleanup barrier")
    manager.serviceUnregistered(replacement)
    checkEquals(1, stop.resolutions.size, "generic stop resolves after terminal callback")
    checkEquals(1, duplicateStop.resolutions.size, "duplicate stop resolves after cleanup")

    // A synchronous unregister exception means Android did not accept the
    // cleanup request. The plugin keeps ownership, retries once internally,
    // and still waits for the terminal callback before resolving stop.
    val retryStart = Invoke(acceptedArgs)
    plugin.advertise_start(retryStart)
    val retryRegistration = manager.registrationCalls.last()
    retryRegistration.listener.onServiceRegistered(retryRegistration.info)
    val retryId = retryStart.resolutions.single()!!["advertiseId"] as Long
    manager.failNextUnregister = true
    val retryStop = Invoke(advertiseStopArgs(retryId))
    plugin.advertise_stop(retryStop)
    checkEquals(0, retryStop.completionCount, "generic stop survives unregister dispatch failure")
    checkThat(
        manager.isUnregistrationPending(retryRegistration.listener),
        "generic stop retries an unaccepted unregister while retaining ownership"
    )
    manager.serviceUnregistered(retryRegistration)
    checkEquals(1, retryStop.resolutions.size, "retried generic stop awaits terminal cleanup")

    // Stop wins a race with an update that is already unregistering. The
    // outer handle remains closed, no replacement is launched, and cleanup is
    // not reported before Android's terminal callback.
    val racingStart = Invoke(acceptedArgs)
    plugin.advertise_start(racingStart)
    val racingInitial = manager.registrationCalls.last()
    racingInitial.listener.onServiceRegistered(racingInitial.info)
    val racingId = racingStart.resolutions.single()!!["advertiseId"] as Long
    val racingUpdate = Invoke(genericAdvertiseUpdateArgs(racingId, port = 9191))
    plugin.advertise_update(racingUpdate)
    val registrationsBeforeStop = manager.registrationCalls.size
    val racingStop = Invoke(advertiseStopArgs(racingId))
    plugin.advertise_stop(racingStop)
    checkEquals(1, racingUpdate.rejections.size, "generic stop rejects in-flight update once")
    checkEquals(0, racingStop.completionCount, "generic racing stop awaits terminal callback")
    manager.serviceUnregistered(racingInitial)
    checkEquals(registrationsBeforeStop, manager.registrationCalls.size, "generic stop prevents replacement")
    checkEquals(1, racingStop.resolutions.size, "generic racing stop resolves after cleanup")

    // A terminal unregistration failure is surfaced to every close waiter and
    // never retries the invalid listener under either AOSP callback ordering.
    val failureStart = Invoke(acceptedArgs)
    plugin.advertise_start(failureStart)
    val failureRegistration = manager.registrationCalls.last()
    failureRegistration.listener.onServiceRegistered(failureRegistration.info)
    val failureId = failureStart.resolutions.single()!!["advertiseId"] as Long
    val failedStop = Invoke(advertiseStopArgs(failureId))
    plugin.advertise_stop(failedStop)
    checkEquals(0, failedStop.completionCount, "failed generic stop waits for callback")
    manager.unregistrationFailed(failureRegistration, 44)
    checkEquals(1, failedStop.rejections.size, "generic stop surfaces cleanup failure")

    val updateFailureStart = Invoke(acceptedArgs)
    plugin.advertise_start(updateFailureStart)
    val updateFailureInitial = manager.registrationCalls.last()
    updateFailureInitial.listener.onServiceRegistered(updateFailureInitial.info)
    val updateFailureId = updateFailureStart.resolutions.single()!!["advertiseId"] as Long
    val failedUpdate = Invoke(genericAdvertiseUpdateArgs(updateFailureId, port = 9292))
    plugin.advertise_update(failedUpdate)
    manager.serviceUnregistered(updateFailureInitial)
    val failedReplacement = manager.registrationCalls.last()
    manager.registrationFailed(failedReplacement, 45)
    checkEquals(1, failedUpdate.rejections.size, "generic replacement failure rejects update")
    val terminalStop = Invoke(advertiseStopArgs(updateFailureId))
    plugin.advertise_stop(terminalStop)
    checkEquals(1, terminalStop.resolutions.size, "terminal generic update still closes cleanly")
}

private fun testThreadedUpdateStopRace() {
    val (plugin, manager) = newPlugin()
    val (id, initial) = startGenericAdvertisement(plugin, manager)
    val unregisterEntered = CountDownLatch(1)
    val unregisterRelease = CountDownLatch(1)
    manager.unregisterEntered = unregisterEntered
    manager.unregisterRelease = unregisterRelease

    val update = Invoke(
        genericAdvertiseUpdateArgs(id, port = 1, txt = peerShapedTxt("10.0.0.7:5005"))
    )
    val stop = Invoke(advertiseStopArgs(id))
    val failure = AtomicReference<Throwable?>(null)
    val updateThread = Thread {
        try {
            plugin.advertise_update(update)
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
            plugin.advertise_stop(stop)
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
    checkEquals(0, stop.completionCount, "racing stop waits for native cleanup")
    manager.serviceUnregistered(initial)
    checkEquals(1, stop.completionCount, "racing stop resolves exactly once")
}

private fun testAospListenerLifecycleAcrossApiEras() {
    for (timing in TerminalRemovalTiming.values()) {
        val (plugin, manager) = newPlugin(timing)

        val failedBrowse = Invoke(genericBrowseArgs())
        plugin.browse_start(failedBrowse)
        val failedBrowseCall = manager.discoveryCalls.last()
        manager.startDiscoveryFailed(failedBrowseCall, 31)
        checkEquals(1, failedBrowse.rejections.size, "$timing browse failure rejects once")
        checkThat(
            !manager.stoppedDiscoveryListeners.contains(failedBrowseCall.listener),
            "$timing start failure never stops a terminal listener"
        )

        val (browseId, browseListener) = startGenericBrowse(plugin, manager)
        val browseStop = Invoke(stopArgs(browseId))
        plugin.browse_stop(browseStop)
        checkEquals(0, browseStop.completionCount, "$timing browse stop waits for terminal callback")
        manager.discoveryStopped(manager.discoveryCalls.last())
        checkEquals(1, browseStop.resolutions.size, "$timing browse stop resolves terminally")
        checkThat(
            !manager.isDiscoveryListenerActive(browseListener),
            "$timing terminal browse callback retires the listener"
        )

        val (id, initial) = startGenericAdvertisement(plugin, manager)
        val update = Invoke(
            genericAdvertiseUpdateArgs(id, port = 1, txt = peerShapedTxt("10.0.0.20:6000"))
        )
        plugin.advertise_update(update)
        manager.serviceUnregistered(initial)
        val replacement = manager.registrationCalls.last()
        replacement.listener.onServiceRegistered(replacement.info)
        checkEquals(1, update.resolutions.size, "$timing update resolves once")

        plugin.advertise_stop(Invoke(advertiseStopArgs(id)))
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
    val (retryId, retryInitial) = startGenericAdvertisement(retryPlugin, retryManager)
    val retryUpdate = Invoke(
        genericAdvertiseUpdateArgs(retryId, port = 1, txt = peerShapedTxt("10.0.0.21:6001"))
    )
    retryPlugin.advertise_update(retryUpdate)
    retryManager.serviceUnregistered(retryInitial)
    val pendingReplacement = retryManager.registrationCalls.last()
    val successfulUnregistersBeforeStop = retryManager.unregisteredListeners.size
    retryManager.failNextUnregister = true
    val retryStop = Invoke(advertiseStopArgs(retryId))
    retryPlugin.advertise_stop(retryStop)
    checkEquals(
        successfulUnregistersBeforeStop,
        retryManager.unregisteredListeners.size,
        "failed dispatch is not reported as an issued unregister"
    )
    checkEquals(0, retryStop.completionCount, "late-registration stop retains its cleanup waiter")
    retryManager.failNextUnregister = true
    pendingReplacement.listener.onServiceRegistered(pendingReplacement.info)
    checkEquals(
        successfulUnregistersBeforeStop + 1,
        retryManager.unregisteredListeners.size,
        "late registration retries cleanup after both dispatch failures"
    )
    checkEquals(0, retryStop.completionCount, "late-registration retry awaits terminal cleanup")
    retryManager.serviceUnregistered(pendingReplacement)
    checkEquals(1, retryUpdate.rejections.size, "stop rejects the pending update once")
    checkEquals(1, retryStop.resolutions.size, "late-registration stop resolves after cleanup")

    // Conversely, a registration-failure callback is terminal in both AOSP
    // orderings. Stop must not pass its already-retired listener back to NSD.
    val (failurePlugin, failureManager) = newPlugin()
    val (failureId, failureInitial) = startGenericAdvertisement(failurePlugin, failureManager)
    val failedReplacementUpdate = Invoke(
        genericAdvertiseUpdateArgs(failureId, port = 1, txt = peerShapedTxt("10.0.0.22:6002"))
    )
    failurePlugin.advertise_update(failedReplacementUpdate)
    failureManager.serviceUnregistered(failureInitial)
    val failedReplacement = failureManager.registrationCalls.last()
    failureManager.registrationFailed(failedReplacement, 32)
    val unregistersAtFailure = failureManager.unregisteredListeners.size
    failurePlugin.advertise_stop(Invoke(advertiseStopArgs(failureId)))
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

private fun testPeerSpecificCommandSurfaceIsRemoved() {
    val commandNames = IrohHttpPlugin::class.java.methods.map { it.name }.toSet()
    for (removed in listOf(
        "browse_peers_start",
        "browse_peers_poll",
        "browse_peers_stop",
        "advertise_peer_start",
        "advertise_peer_update",
        "advertise_peer_stop"
    )) {
        checkThat(removed !in commandNames, "$removed must be removed in favor of generic DNS-SD")
    }
}

fun main() {
    testBrowseReadinessAndTerminalConsumption()
    testGenericBrowseRecordsPresenceAndRepeatedUpserts()
    testRetiredResolveQueuesDoNotStarveNewSessions()
    testAdvertisementLifecycleAndRaces()
    testGenericAdvertiseContract()
    testThreadedUpdateStopRace()
    testAospListenerLifecycleAcrossApiEras()
    testPeerSpecificCommandSurfaceIsRemoved()
    println("Android generic discovery contract: 7/7 test groups passed")
}
