package com.iroh.http

import android.app.Activity
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ext.SdkExtensions
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONArray
import org.json.JSONObject
import java.net.InetAddress
import java.net.Inet6Address
import java.net.NetworkInterface
import java.nio.charset.StandardCharsets
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

@InvokeArg
class BrowsePollArgs {
    var browseId: Long = 0
}

@InvokeArg
class BrowseStopArgs {
    var browseId: Long = 0
}

@InvokeArg
class AdvertiseStopArgs {
    var advertiseId: Long = 0
}

// Generic DNS-SD (arbitrary services, not iroh peers).

@InvokeArg
class DnsSdAdvertiseStartArgs {
    lateinit var serviceName: String
    lateinit var instanceName: String
    var port: Int = 0
    var protocol: String = "udp"
    var addrs: List<String> = emptyList()
    var txt: Map<String, String> = emptyMap()
}

@InvokeArg
class DnsSdAdvertiseUpdateArgs {
    var advertiseId: Long = 0
    var port: Int = 0
    var addrs: List<String> = emptyList()
    var txt: Map<String, String> = emptyMap()
}

@InvokeArg
class DnsSdBrowseStartArgs {
    lateinit var serviceName: String
    var protocol: String = "udp"
}

@TauriPlugin
class IrohHttpPlugin(private val activity: Activity) : Plugin(activity) {

    private companion object {
        const val MAX_UNREGISTER_DISPATCH_ATTEMPTS = 2
        const val MAX_BROWSE_STOP_DISPATCH_ATTEMPTS = 2
        const val INITIAL_STOP_RETRY_DELAY_MILLIS = 25L
        const val MAX_STOP_RETRY_DELAY_MILLIS = 1_000L
        const val RESOLVE_TIMEOUT_MILLIS = 5_000L
        const val MAX_RESOLVER_ROTATIONS = 4
    }

    private val nextBrowseId = AtomicLong(1)
    private val nextAdvertiseId = AtomicLong(1)
    private val lifecycleHandler = Handler(Looper.getMainLooper())
    private val resolverRecoveryExecutor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "iroh-http-nsd-recovery").apply { isDaemon = true }
    }

    private enum class NativeSessionState(val pollValue: String) {
        STARTING("active"),
        ACTIVE("active"),
        CLOSED("closed"),
        FAILED("failed")
    }

    /** Complete an asynchronous Tauri command at most once across races. */
    private class InvokeOnce(private val invoke: Invoke) {
        private val completed = AtomicBoolean(false)

        fun resolve(payload: JSObject) {
            if (completed.compareAndSet(false, true)) invoke.resolve(payload)
        }

        fun resolve() {
            if (completed.compareAndSet(false, true)) invoke.resolve()
        }

        fun reject(message: String) {
            if (completed.compareAndSet(false, true)) invoke.reject(message)
        }
    }

    private enum class AdvertisementUpdatePhase {
        UNREGISTERING,
        REGISTERING
    }

    private inner class NsdMulticastLease {
        private val released = AtomicBoolean(false)

        fun release() {
            if (released.compareAndSet(false, true)) releaseNsdMulticastLease()
        }
    }

    private data class AdvertisementUpdate(
        val info: NsdServiceInfo,
        val invoke: InvokeOnce,
        var phase: AdvertisementUpdatePhase = AdvertisementUpdatePhase.UNREGISTERING
    )

    private class AdvertiseSession(
        val id: Long,
        val manager: NsdManager,
        val startInvoke: InvokeOnce,
        val multicastLease: NsdMulticastLease?,
        var state: NativeSessionState = NativeSessionState.STARTING,
        var terminalError: String? = null,
        var pendingStartFailure: String? = null,
        var generation: Long = 1,
        var pendingUpdate: AdvertisementUpdate? = null,
        val pendingStops: MutableList<InvokeOnce> = mutableListOf(),
        // One unregister request is allowed per generation. AOSP keeps the
        // listener mapped while unregisterService() dispatches, then retires
        // it at the terminal callback boundary (after the callback on API 21,
        // before it on current Android). Registration failure retires it at
        // the analogous callback boundary too. Either way, a terminally
        // retired listener must never be retried or reused.
        val retiredRegistrationGenerations: MutableSet<Long> = mutableSetOf(),
        // Unregister was accepted, but Android has not delivered its terminal
        // callback yet. This is the native cleanup acknowledgement barrier.
        val unregisteringGenerations: MutableSet<Long> = mutableSetOf(),
        var stopRetryScheduled: Boolean = false,
        var stopRetryDelayMillis: Long = INITIAL_STOP_RETRY_DELAY_MILLIS
    ) {
        lateinit var listener: NsdManager.RegistrationListener
        lateinit var info: NsdServiceInfo
    }

    /** A DNS-SD browse session carrying complete service records. */
    private class DnsSdBrowseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.DiscoveryListener,
        val serviceType: String,
        val startInvoke: InvokeOnce,
        val multicastLease: NsdMulticastLease?,
        var state: NativeSessionState = NativeSessionState.STARTING,
        var terminalError: String? = null,
        var pendingStartFailure: String? = null,
        val pendingRecords: MutableList<JSObject> = mutableListOf(),
        // Instances that have produced at least one resolved record. This is
        // presence bookkeeping for removals, not record de-duplication: generic
        // browse exposes every platform announcement, including identical ones.
        val knownInstances: MutableSet<String> = mutableSetOf(),
        val presenceGenerations: MutableMap<String, Long> = mutableMapOf(),
        var nextPresenceGeneration: Long = 1,
        val pendingStops: MutableList<InvokeOnce> = mutableListOf(),
        var readinessPending: Boolean = true,
        var stopDispatchAccepted: Boolean = false,
        var stopDeferredUntilReady: Boolean = false,
        var nativeTerminal: Boolean = false,
        var stopRetryScheduled: Boolean = false,
        var stopRetryDelayMillis: Long = INITIAL_STOP_RETRY_DELAY_MILLIS
    )

    /** Provenance for one serialized legacy NsdManager resolve request. */
    private data class ResolveOwner(
        val session: DnsSdBrowseSession,
        val instanceName: String,
        val presenceGeneration: Long
    )

    private val advertiseMap = ConcurrentHashMap<Long, AdvertiseSession>()
    private val dnsSdBrowseMap = ConcurrentHashMap<Long, DnsSdBrowseSession>()
    private val multicastLockGuard = Any()
    private var sharedMulticastLock: WifiManager.MulticastLock? = null
    private var multicastLeaseCount = 0

    private fun nsd(): NsdManager? =
        activity.getSystemService(Context.NSD_SERVICE) as? NsdManager

    /**
     * Older Android NSD implementations require an explicit multicast lock,
     * even for a foreground app. Share one lock across active browse and
     * advertisement sessions, and avoid taking it once T extension 7 lets the
     * system manage foreground multicast automatically.
     */
    private fun requiresLegacyMulticastLock(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            SdkExtensions.getExtensionVersion(Build.VERSION_CODES.TIRAMISU) < 7

    private fun acquireNsdMulticastLease(): NsdMulticastLease? {
        if (!requiresLegacyMulticastLock()) return null
        val wifi = activity.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            ?: return null
        synchronized(multicastLockGuard) {
            if (multicastLeaseCount == 0) {
                val lock = wifi.createMulticastLock("iroh-http-dnssd")
                try {
                    lock.setReferenceCounted(false)
                    lock.acquire()
                } catch (error: Throwable) {
                    if (lock.isHeld) lock.release()
                    throw error
                }
                sharedMulticastLock = lock
            }
            multicastLeaseCount += 1
        }
        return NsdMulticastLease()
    }

    private fun releaseNsdMulticastLease() {
        synchronized(multicastLockGuard) {
            if (multicastLeaseCount == 0) return
            multicastLeaseCount -= 1
            if (multicastLeaseCount == 0) {
                sharedMulticastLock?.let { lock ->
                    if (lock.isHeld) lock.release()
                }
                sharedMulticastLock = null
            }
        }
    }

    private fun removeAdvertisementSession(session: AdvertiseSession) {
        advertiseMap.remove(session.id, session)
        session.multicastLease?.release()
    }

    private fun removeBrowseSession(session: DnsSdBrowseSession) {
        dnsSdBrowseMap.remove(session.id, session)
        session.multicastLease?.release()
    }

    /**
     * Format a DNS server for Rust without losing an IPv6 routing scope.
     *
     * Android 21–29 can return a link-local Inet6Address with scopeId=0 even
     * though LinkProperties still identifies the owning interface. In that
     * case the caller supplies the interface's numeric index. Returning an
     * unscoped fe80:: address would configure a resolver that cannot route any
     * query, so the missing-scope case fails explicitly.
     */
    private fun dnsServerLiteral(address: InetAddress, interfaceIndex: Int?): String {
        val host = address.hostAddress?.substringBefore('%')
            ?: throw IllegalArgumentException("DNS server has no numeric address")
        if (address !is Inet6Address) return host

        val addressScope = address.scopeId.takeIf { it > 0 }
        val scope = addressScope ?: interfaceIndex?.takeIf { it > 0 }
        if (address.isLinkLocalAddress) {
            requireNotNull(scope) {
                "link-local DNS server $host has no resolvable interface scope"
            }
            return "$host%$scope"
        }
        return addressScope?.let { "$host%$it" } ?: host
    }

    /** Return a dialable interface IP literal, excluding addresses Rust rejects. */
    private fun interfaceAddressLiteral(address: InetAddress): String? {
        if (
            address.isAnyLocalAddress ||
            address.isLoopbackAddress ||
            address.isLinkLocalAddress ||
            address.isMulticastAddress
        ) {
            return null
        }
        return address.hostAddress?.substringBefore('%')
    }

    /** Add addresses only while their owning interface is operational. */
    private fun addOperationalInterfaceAddresses(
        output: MutableSet<String>,
        networkInterface: NetworkInterface,
        addresses: Iterable<InetAddress>
    ) {
        try {
            if (!networkInterface.isUp || networkInterface.isLoopback) return
            for (address in addresses) {
                interfaceAddressLiteral(address)?.let(output::add)
            }
        } catch (e: Throwable) {
            Log.w(
                "iroh-http-network",
                "could not inspect interface ${networkInterface.name}: ${e.message}"
            )
        }
    }

    // ── DNS ───────────────────────────────────────────────────────────────────

    /**
     * Return the active network's configured DNS servers (IP strings).
     *
     * iroh's Rust DNS resolver cannot read Android's system DNS config (there is
     * no `/etc/resolv.conf`; servers live in `LinkProperties`, reachable only
     * via JNI/`ndk_context`). The Rust side calls this at endpoint creation and
     * configures iroh's resolver with the returned servers so relay, pkarr, and
     * DNS-discovery lookups resolve instead of timing out. Android must return
     * at least one usable server: resolving an empty list would make Rust take
     * the same unavailable default-resolver path this bridge exists to avoid.
     */
    @Command
    fun get_dns_servers(invoke: Invoke) {
        val servers = JSONArray()
        val rejected = mutableListOf<String>()
        try {
            val cm = activity.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            if (cm != null) {
                val networks: List<Network> =
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                        // getActiveNetwork() is API 23+. On older devices it
                        // throws NoSuchMethodError (a linkage Error, NOT an
                        // Exception), which would otherwise crash endpoint
                        // creation on the declared minSdk=21 range (#350 F2).
                        cm.activeNetwork?.let { listOf(it) } ?: emptyList()
                    } else {
                        // API 21/22 fallback: allNetworks (added in API 21).
                        @Suppress("DEPRECATION")
                        cm.allNetworks.toList()
                }
                for (network in networks) {
                    val props = cm.getLinkProperties(network) ?: continue
                    val interfaceIndex = try {
                        props.interfaceName
                            ?.let { NetworkInterface.getByName(it) }
                            ?.index
                            ?.takeIf { it > 0 }
                    } catch (e: Throwable) {
                        Log.w(
                            "iroh-http-dns",
                            "could not resolve interface index for ${props.interfaceName}: ${e.message}"
                        )
                        null
                    }
                    for (addr in props.dnsServers) {
                        try {
                            servers.put(dnsServerLiteral(addr, interfaceIndex))
                        } catch (e: IllegalArgumentException) {
                            rejected.add(e.message ?: "unusable DNS server $addr")
                        }
                    }
                }
            }
        } catch (e: Throwable) {
            // Catch linkage/verification errors as well as ordinary failures,
            // but surface them: silently returning [] would select iroh's
            // known-broken Android default-resolver path.
            val message = "get_dns_servers failed: ${e.message}"
            if (servers.length() == 0) {
                Log.e("iroh-http-dns", message)
                return invoke.reject(message)
            }
            Log.w("iroh-http-dns", "$message; continuing with collected servers")
        }
        if (servers.length() == 0) {
            val details = if (rejected.isEmpty()) {
                "Android reported no DNS servers for any active network"
            } else {
                "all platform DNS servers were unusable: ${rejected.joinToString("; ")}"
            }
            return invoke.reject(details)
        }
        if (rejected.isNotEmpty()) {
            Log.w(
                "iroh-http-dns",
                "ignoring unusable DNS server(s): ${rejected.joinToString("; ")}"
            )
        }
        val ret = JSObject()
        ret.put("servers", servers)
        invoke.resolve(ret)
    }

    /**
     * Return every usable address on an operational Android interface.
     *
     * `if-addrs` 0.15 cannot be linked on this plugin's minSdk 21–23 because
     * Android introduced libc `getifaddrs` only in API 24. Both APIs below are
     * available on API 21: LinkProperties supplies active-network addresses,
     * while NetworkInterface enumeration also retains physical LAN interfaces
     * hidden behind a VPN/default network.
     */
    @Command
    fun get_interface_addresses(invoke: Invoke) {
        val addresses = linkedSetOf<String>()

        try {
            val cm = activity.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            if (cm != null) {
                @Suppress("DEPRECATION")
                for (network in cm.allNetworks) {
                    val props = cm.getLinkProperties(network) ?: continue
                    val interfaceName = props.interfaceName ?: continue
                    val networkInterface = try {
                        NetworkInterface.getByName(interfaceName)
                    } catch (e: Throwable) {
                        Log.w(
                            "iroh-http-network",
                            "could not resolve interface $interfaceName: ${e.message}"
                        )
                        null
                    } ?: continue
                    addOperationalInterfaceAddresses(
                        addresses,
                        networkInterface,
                        props.linkAddresses.map { it.address }
                    )
                }
            }
        } catch (e: Throwable) {
            Log.w("iroh-http-network", "LinkProperties inventory failed: ${e.message}")
        }

        try {
            val interfaces = NetworkInterface.getNetworkInterfaces()
            while (interfaces != null && interfaces.hasMoreElements()) {
                val networkInterface = interfaces.nextElement()
                val inetAddresses = mutableListOf<InetAddress>()
                val values = networkInterface.inetAddresses
                while (values.hasMoreElements()) inetAddresses.add(values.nextElement())
                addOperationalInterfaceAddresses(addresses, networkInterface, inetAddresses)
            }
        } catch (e: Throwable) {
            Log.w("iroh-http-network", "NetworkInterface inventory failed: ${e.message}")
        }

        val ret = JSObject()
        ret.put("addresses", JSONArray(addresses.toList()))
        invoke.resolve(ret)
    }

    // ── Resolve queue ────────────────────────────────────────────────────────
    //
    // `NsdManager` allows only one outstanding `resolveService()` call at a
    // time; a second concurrent call fails with `FAILURE_ALREADY_ACTIVE` and
    // `onResolveFailed` is effectively a silent no-op, so records get dropped
    // whenever several services appear together. Every browse session shares
    // this queue so resolves across handles are serialized too.
    private data class ResolveRequest(
        val owner: ResolveOwner,
        var manager: NsdManager,
        val serviceInfo: NsdServiceInfo,
        val listener: NsdManager.ResolveListener,
        val completed: AtomicBoolean = AtomicBoolean(false),
        var timeoutTask: Runnable? = null
    )

    private val resolveQueue = java.util.ArrayDeque<ResolveRequest>()
    private var resolveInProgress = false
    private var activeResolve: ResolveRequest? = null
    private var resolverManager: NsdManager? = null
    private var resolverRotationCount = 0
    /** Process-lifetime terminal state: legacy NsdManager clients cannot be closed. */
    private var resolverTerminalError: String? = null

    private fun enqueueResolve(
        owner: ResolveOwner,
        manager: NsdManager,
        serviceInfo: NsdServiceInfo,
        listener: NsdManager.ResolveListener
    ) {
        synchronized(resolveQueue) {
            resolveQueue.addLast(ResolveRequest(owner, manager, serviceInfo, listener))
            if (resolveInProgress) return
            resolveInProgress = true
        }
        drainResolveQueue()
    }

    private fun isCurrentResolveOwner(owner: ResolveOwner): Boolean =
        synchronized(owner.session) {
            dnsSdBrowseMap[owner.session.id] === owner.session &&
                owner.session.state == NativeSessionState.ACTIVE &&
                owner.session.presenceGenerations[owner.instanceName] ==
                owner.presenceGeneration
        }

    /** Drop queued work for a retired session without disturbing another owner. */
    private fun retireResolveRequests(session: Any) {
        synchronized(resolveQueue) {
            val iterator = resolveQueue.iterator()
            while (iterator.hasNext()) {
                if (iterator.next().owner.session === session) iterator.remove()
            }
        }
    }

    /**
     * Retire queued duplicates for a vanished presence. Its active native
     * resolve retains the single-flight slot until callback or watchdog: loss
     * is normal DNS-SD churn and is not proof that the resolver client stalled.
     */
    private fun retireResolveRequest(owner: ResolveOwner) {
        synchronized(resolveQueue) {
            val iterator = resolveQueue.iterator()
            while (iterator.hasNext()) {
                if (iterator.next().owner == owner) iterator.remove()
            }
        }
    }

    private fun claimResolve(request: ResolveRequest): Boolean {
        if (!request.completed.compareAndSet(false, true)) return false
        request.timeoutTask?.let { lifecycleHandler.removeCallbacks(it) }
        request.timeoutTask = null
        return true
    }

    private fun finishClaimedResolve(request: ResolveRequest, callback: (() -> Unit)? = null) {
        try {
            callback?.invoke()
        } finally {
            synchronized(resolveQueue) {
                if (activeResolve === request) activeResolve = null
            }
            drainResolveQueue()
        }
    }

    private fun completeResolve(request: ResolveRequest, callback: (() -> Unit)? = null) {
        if (!claimResolve(request)) return
        finishClaimedResolve(request, callback)
    }

    /**
     * Replace a legacy resolver client whose native single-flight slot may be
     * poisoned. A configuration context owns a separate system-service cache,
     * so Android creates a new NsdManager client instead of returning the
     * instance whose unresolved request can no longer be cancelled on API 21–33.
     */
    private fun freshResolverManager(): NsdManager? = try {
        activity
            .createConfigurationContext(activity.resources.configuration)
            .getSystemService(Context.NSD_SERVICE) as? NsdManager
    } catch (error: Throwable) {
        Log.w("iroh-http-mdns", "failed to create fresh resolver client: ${error.message}")
        null
    }

    private fun abandonResolve(request: ResolveRequest) {
        if (!claimResolve(request)) return
        resolverRecoveryExecutor.execute {
            val canRotate = synchronized(resolveQueue) {
                resolverRotationCount < MAX_RESOLVER_ROTATIONS
            }
            val replacement = if (canRotate) freshResolverManager() else null
            val installed = synchronized(resolveQueue) {
                if (resolverManager !== request.manager || replacement == null) {
                    false
                } else {
                    resolverManager = replacement
                    resolverRotationCount += 1
                    true
                }
            }
            if (installed) {
                finishClaimedResolve(request)
            } else {
                failResolverSessions(
                    if (canRotate) {
                        "Android DNS-SD resolver recovery could not create a fresh client"
                    } else {
                        "Android DNS-SD resolver recovery exhausted after " +
                            "$MAX_RESOLVER_ROTATIONS recovery rotations; restart the app"
                    }
                )
            }
        }
    }

    private fun failResolverSessions(message: String) {
        val sessions = synchronized(resolveQueue) {
            resolverTerminalError = message
            val affected = dnsSdBrowseMap.values.toSet()
            resolveQueue.clear()
            activeResolve = null
            resolveInProgress = false
            affected
        }
        Log.e("iroh-http-mdns", message)
        sessions.forEach { session ->
            synchronized(session) {
                if (
                    dnsSdBrowseMap[session.id] === session &&
                    (session.state == NativeSessionState.STARTING ||
                        session.state == NativeSessionState.ACTIVE)
                ) {
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    session.terminalError = message
                    if (failedBeforeReady) {
                        // The native discovery was admitted before exhaustion,
                        // but Rust has not received its handle yet. Fence that
                        // start until Android acknowledges listener cleanup.
                        session.state = NativeSessionState.CLOSED
                        session.pendingStartFailure = message
                        retireResolveRequests(session)
                        val cleanupError = dispatchBrowseStopWithRetry(session)
                        if (cleanupError != null) {
                            session.stopDeferredUntilReady = true
                            session.terminalError =
                                "$message; cleanup: ${cleanupError.message}"
                        }
                    } else {
                        session.state = NativeSessionState.FAILED
                    }
                }
            }
        }
    }

    private fun drainResolveQueue() {
        var request: ResolveRequest
        while (true) {
            val next = synchronized(resolveQueue) {
                resolveQueue.pollFirst().also { candidate ->
                    activeResolve = candidate
                    if (candidate == null) resolveInProgress = false
                }
            }
            if (next == null) return
            if (isCurrentResolveOwner(next.owner)) {
                request = next
                break
            }
            next.completed.set(true)
            synchronized(resolveQueue) {
                if (activeResolve === next) activeResolve = null
            }
        }
        request.manager = synchronized(resolveQueue) {
            resolverManager ?: request.manager.also { resolverManager = it }
        }
        val manager = request.manager
        val serviceInfo = request.serviceInfo
        val listener = request.listener
        // #334: greppable trace of the serialized resolve queue. `depth` is the
        // number of resolves still queued behind this one — proof that
        // concurrent resolves are serialized rather than dropped with
        // FAILURE_ALREADY_ACTIVE.
        val depth = synchronized(resolveQueue) { resolveQueue.size }
        Log.d(
            "IROH_DNSSD_CHECK",
            "resolve dequeue instance=${serviceInfo.serviceName} depth=$depth",
        )
        fun finish(callback: () -> Unit) {
            completeResolve(request, callback)
        }
        val wrapped = object : NsdManager.ResolveListener {
            override fun onServiceResolved(resolved: NsdServiceInfo) {
                Log.d(
                    "IROH_DNSSD_CHECK",
                    "resolve ok instance=${resolved.serviceName} port=${resolved.port}",
                )
                finish { listener.onServiceResolved(resolved) }
            }
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.d(
                    "IROH_DNSSD_CHECK",
                    "resolve fail instance=${serviceInfo.serviceName} errorCode=$errorCode",
                )
                finish { listener.onResolveFailed(serviceInfo, errorCode) }
            }
        }
        val timeoutTask = Runnable {
            if (request.completed.get()) return@Runnable
            Log.w(
                "iroh-http-mdns",
                "resolve timed out for ${serviceInfo.serviceName}; rotating resolver client"
            )
            abandonResolve(request)
        }
        request.timeoutTask = timeoutTask
        lifecycleHandler.postDelayed(timeoutTask, RESOLVE_TIMEOUT_MILLIS)
        try {
            if (request.completed.get() || !isCurrentResolveOwner(request.owner)) {
                completeResolve(request)
                return
            }
            synchronized(resolveQueue) {
                if (request.completed.get()) return
                manager.resolveService(serviceInfo, wrapped)
            }
        } catch (e: Throwable) {
            Log.w(
                "iroh-http-mdns",
                "resolve threw for ${serviceInfo.serviceName}: ${e.message}"
            )
            finish { listener.onResolveFailed(serviceInfo, -1) }
        }
    }

    // ── Advertise ─────────────────────────────────────────────────────────────

    /**
     * Dispatch unregister for one native listener at most once.
     *
     * Mark the generation only after the call returns. `registerService()`
     * installs the listener in NsdManager's map synchronously before sending
     * the asynchronous request, but an immediate platform exception still
     * means no unregister was issued. Marking first would suppress the late
     * `onServiceRegistered` cleanup that handles an uncertain platform race.
     * Terminal callbacks mark the generation independently because AOSP has
     * retired the listener by that callback boundary.
     */
    private fun unregisterRegistrationOnce(
        session: AdvertiseSession,
        generation: Long,
        listener: NsdManager.RegistrationListener
    ): Boolean {
        if (generation in session.retiredRegistrationGenerations) return false
        session.manager.unregisterService(listener)
        session.retiredRegistrationGenerations.add(generation)
        session.unregisteringGenerations.add(generation)
        return true
    }

    /**
     * Retry a synchronous dispatch rejection once without surrendering native
     * ownership. A thrown call was not accepted by NsdManager, so the listener
     * remains eligible for another attempt.
     */
    private fun unregisterRegistrationWithRetry(
        session: AdvertiseSession,
        generation: Long,
        listener: NsdManager.RegistrationListener
    ): Throwable? {
        var lastError: Throwable? = null
        repeat(MAX_UNREGISTER_DISPATCH_ATTEMPTS) {
            try {
                unregisterRegistrationOnce(session, generation, listener)
                return null
            } catch (error: Throwable) {
                lastError = error
            }
        }
        return lastError
    }

    /** Complete every waiter only after the native registration is terminal. */
    private fun finishAdvertisementStop(session: AdvertiseSession, error: String? = null) {
        removeAdvertisementSession(session)
        val waiters = session.pendingStops.toList()
        session.pendingStops.clear()
        for (waiter in waiters) {
            if (error == null) waiter.resolve() else waiter.reject(error)
        }
    }

    /**
     * Retry an unaccepted unregister without requiring another Tauri command.
     *
     * Rust closes each native handle exactly once. Keep that Invoke pending and
     * retain the listener until Android accepts unregister and later reports a
     * terminal callback. The capped delay avoids a busy loop while still
     * recovering from transient framework/OEM dispatch failures.
     */
    private fun scheduleAdvertisementStopRetry(session: AdvertiseSession, error: Throwable) {
        if (
            session.stopRetryScheduled ||
            advertiseMap[session.id] !== session ||
            session.state != NativeSessionState.CLOSED
        ) return
        session.stopRetryScheduled = true
        val delayMillis = session.stopRetryDelayMillis
        session.stopRetryDelayMillis =
            (delayMillis * 2).coerceAtMost(MAX_STOP_RETRY_DELAY_MILLIS)
        Log.w(
            "iroh-http-dnssd",
            "advertise ${session.id} unregister dispatch failed; retrying in " +
                "${delayMillis}ms: ${error.message}"
        )
        lifecycleHandler.postDelayed({
            synchronized(session) {
                session.stopRetryScheduled = false
                if (
                    advertiseMap[session.id] !== session ||
                    session.state != NativeSessionState.CLOSED ||
                    session.generation in session.unregisteringGenerations ||
                    session.generation in session.retiredRegistrationGenerations
                ) return@synchronized

                val retryError = unregisterRegistrationWithRetry(
                    session,
                    session.generation,
                    session.listener
                )
                if (retryError == null) {
                    session.stopRetryDelayMillis = INITIAL_STOP_RETRY_DELAY_MILLIS
                } else {
                    scheduleAdvertisementStopRetry(session, retryError)
                }
            }
        }, delayMillis)
    }

    private fun rejectPendingAdvertisementStart(
        session: AdvertiseSession,
        cleanupError: String? = null
    ) {
        val startError = session.pendingStartFailure ?: return
        session.pendingStartFailure = null
        session.startInvoke.reject(
            cleanupError?.let { "$startError; native cleanup failed: $it" } ?: startError
        )
    }

    private fun registrationListener(
        session: AdvertiseSession,
        generation: Long
    ): NsdManager.RegistrationListener = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
            synchronized(session) {
                if (advertiseMap[session.id] !== session) return
                if (session.state == NativeSessionState.CLOSED) {
                    // A stop can win while Android is still completing an
                    // asynchronous register. Tear down that late registration
                    // instead of leaking a native service with no owner.
                    val error = unregisterRegistrationWithRetry(session, generation, this)
                    if (error != null) {
                        scheduleAdvertisementStopRetry(session, error)
                    }
                    return
                }
                if (session.generation != generation || session.listener !== this) return

                val update = session.pendingUpdate
                if (
                    update != null &&
                    update.phase == AdvertisementUpdatePhase.REGISTERING
                ) {
                    session.pendingUpdate = null
                    session.state = NativeSessionState.ACTIVE
                    update.invoke.resolve()
                    return
                }
                if (session.state == NativeSessionState.STARTING) {
                    session.state = NativeSessionState.ACTIVE
                    val ret = JSObject()
                    ret.put("advertiseId", session.id)
                    session.startInvoke.resolve(ret)
                }
            }
        }

        override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            synchronized(session) {
                // API 21 removes this listener after the callback returns;
                // current Android removes it before dispatching the callback.
                // It is terminal on both and must not be passed to unregister.
                session.retiredRegistrationGenerations.add(generation)
                session.unregisteringGenerations.remove(generation)
                if (
                    advertiseMap[session.id] !== session ||
                    session.generation != generation ||
                    session.listener !== this
                ) return
                if (session.state == NativeSessionState.CLOSED) {
                    session.pendingUpdate?.invoke?.reject("DNS-SD advertisement is closed")
                    session.pendingUpdate = null
                    rejectPendingAdvertisementStart(session)
                    finishAdvertisementStop(session)
                    return
                }
                val message = "DNS-SD registration failed: error $errorCode"
                session.terminalError = message
                val update = session.pendingUpdate
                if (
                    update != null &&
                    update.phase == AdvertisementUpdatePhase.REGISTERING
                ) {
                    session.pendingUpdate = null
                    session.state = NativeSessionState.FAILED
                    update.invoke.reject("DNS-SD advertisement update failed: error $errorCode")
                } else if (session.state == NativeSessionState.STARTING) {
                    session.state = NativeSessionState.FAILED
                    removeAdvertisementSession(session)
                    session.startInvoke.reject(message)
                } else if (session.state == NativeSessionState.ACTIVE) {
                    session.state = NativeSessionState.FAILED
                }
                session.multicastLease?.release()
                Log.e("iroh-http-dnssd", "advertise ${session.id} failed: $errorCode")
            }
        }

        override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {
            synchronized(session) {
                session.retiredRegistrationGenerations.add(generation)
                session.unregisteringGenerations.remove(generation)
                if (
                    advertiseMap[session.id] !== session ||
                    session.generation != generation ||
                    session.listener !== this
                ) return
                if (session.state == NativeSessionState.CLOSED) {
                    rejectPendingAdvertisementStart(session)
                    finishAdvertisementStop(session)
                    return
                }
                val update = session.pendingUpdate
                if (
                    update != null &&
                    update.phase == AdvertisementUpdatePhase.UNREGISTERING
                ) {
                    // API 21 has no in-place NSD TXT update. Preserve the outer
                    // handle and pk while atomically moving the session to a
                    // replacement RegistrationListener.
                    update.phase = AdvertisementUpdatePhase.REGISTERING
                    session.generation += 1
                    val nextGeneration = session.generation
                    session.info = update.info
                    session.listener = registrationListener(session, nextGeneration)
                    try {
                        session.manager.registerService(
                            session.info,
                            NsdManager.PROTOCOL_DNS_SD,
                            session.listener
                        )
                    } catch (e: Throwable) {
                        if (session.pendingUpdate === update) session.pendingUpdate = null
                        session.state = NativeSessionState.FAILED
                        session.terminalError = "Registration failed: ${e.message}"
                        try {
                            unregisterRegistrationOnce(
                                session,
                                session.generation,
                                session.listener
                            )
                        } catch (_: Throwable) {}
                        update.invoke.reject("DNS-SD advertisement update failed: ${e.message}")
                    }
                } else {
                    val message = "DNS-SD registration stopped unexpectedly"
                    session.terminalError = message
                    if (session.state == NativeSessionState.STARTING) {
                        session.state = NativeSessionState.FAILED
                        removeAdvertisementSession(session)
                        session.startInvoke.reject(message)
                    } else if (session.state == NativeSessionState.ACTIVE) {
                        session.state = NativeSessionState.FAILED
                        session.multicastLease?.release()
                    }
                }
            }
        }

        override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            synchronized(session) {
                session.retiredRegistrationGenerations.add(generation)
                session.unregisteringGenerations.remove(generation)
                if (
                    advertiseMap[session.id] !== session ||
                    session.generation != generation ||
                    session.listener !== this
                ) return
                if (session.state == NativeSessionState.CLOSED) {
                    // AOSP has retired this listener mapping at the callback
                    // boundary. Retrying it would throw and can never clean
                    // up the native service.
                    val message = "DNS-SD advertisement stop failed: error $errorCode"
                    session.terminalError = message
                    rejectPendingAdvertisementStart(session, message)
                    finishAdvertisementStop(session, message)
                    return
                }
                val update = session.pendingUpdate
                if (
                    update != null &&
                    update.phase == AdvertisementUpdatePhase.UNREGISTERING
                ) {
                    session.pendingUpdate = null
                    session.state = NativeSessionState.FAILED
                    session.terminalError =
                        "DNS-SD unregistration failed: error $errorCode"
                    update.invoke.reject(
                        "DNS-SD advertisement update could not unregister old record: error $errorCode"
                    )
                } else {
                    session.state = NativeSessionState.FAILED
                    session.terminalError = "DNS-SD unregistration failed: error $errorCode"
                    session.multicastLease?.release()
                }
            }
        }
    }

    private fun startAdvertisement(
        manager: NsdManager,
        advertiseId: Long,
        info: NsdServiceInfo,
        multicastLease: NsdMulticastLease?,
        invoke: Invoke
    ) {
        val session = AdvertiseSession(
            advertiseId,
            manager,
            InvokeOnce(invoke),
            multicastLease
        )
        synchronized(session) {
            session.info = info
            session.listener = registrationListener(session, session.generation)
            advertiseMap[advertiseId] = session
            try {
                manager.registerService(info, NsdManager.PROTOCOL_DNS_SD, session.listener)
            } catch (e: Throwable) {
                val message = "Registration failed: ${e.message}"
                if (e is IllegalArgumentException) {
                    removeAdvertisementSession(session)
                    session.state = NativeSessionState.FAILED
                    session.terminalError = message
                    session.startInvoke.reject(message)
                    return
                }
                session.state = NativeSessionState.CLOSED
                session.terminalError = message
                session.pendingStartFailure = message
                // Validation failures happen before NsdManager installs the
                // listener; transport failures can happen after installation.
                // Retain ownership until cleanup is accepted and terminal. If
                // both bounded attempts fail, a late readiness callback can
                // retry the same still-owned listener.
                val cleanupError = unregisterRegistrationWithRetry(
                    session,
                    session.generation,
                    session.listener
                )
                // If cleanup is still unaccepted, keep start pending and the
                // map owned. Only a terminal native callback may release the
                // Rust start lease.
                cleanupError?.let { session.terminalError = "$message; cleanup: ${it.message}" }
            }
        }
    }

    private fun updateAdvertisement(advertiseId: Long, info: NsdServiceInfo, invoke: Invoke) {
        val completion = InvokeOnce(invoke)
        val session = advertiseMap[advertiseId]
            ?: return completion.reject("DNS-SD advertisement is closed")
        synchronized(session) {
            if (advertiseMap[advertiseId] !== session) {
                return completion.reject("DNS-SD advertisement is closed")
            }
            if (session.state != NativeSessionState.ACTIVE) {
                val reason = session.terminalError ?: "advertisement is not active"
                return completion.reject("DNS-SD advertisement update failed: $reason")
            }
            if (session.pendingUpdate != null) {
                return completion.reject("DNS-SD advertisement update already in progress")
            }
            val update = AdvertisementUpdate(info, completion)
            session.pendingUpdate = update
            try {
                check(
                    unregisterRegistrationOnce(
                        session,
                        session.generation,
                        session.listener
                    )
                ) { "registration listener was already invalidated" }
            } catch (e: Throwable) {
                if (session.pendingUpdate === update) session.pendingUpdate = null
                session.state = NativeSessionState.FAILED
                session.terminalError = "DNS-SD unregistration failed: ${e.message}"
                completion.reject("DNS-SD advertisement update failed: ${e.message}")
            }
        }
    }

    private fun stopAdvertisement(advertiseId: Long, invoke: Invoke) {
        val completion = InvokeOnce(invoke)
        val session = advertiseMap[advertiseId] ?: return completion.resolve()
        synchronized(session) {
            if (advertiseMap[advertiseId] !== session) return completion.resolve()
            session.pendingStops.add(completion)
            val wasStarting = session.state == NativeSessionState.STARTING
            val update = if (session.state == NativeSessionState.CLOSED) {
                null
            } else {
                session.pendingUpdate.also {
                    session.state = NativeSessionState.CLOSED
                    if (wasStarting) {
                        session.startInvoke.reject(
                            "DNS-SD advertisement stopped before becoming ready"
                        )
                    }
                    session.pendingUpdate = null
                    it?.invoke?.reject("DNS-SD advertisement is closed")
                }
            }

            // An UNREGISTERING update already dispatched the terminal call.
            if (update?.phase == AdvertisementUpdatePhase.UNREGISTERING) return
            if (session.generation in session.unregisteringGenerations) return
            if (session.stopRetryScheduled) return
            // A terminal listener has no native registration left to stop.
            if (session.generation in session.retiredRegistrationGenerations) {
                finishAdvertisementStop(session)
                return
            }

            val registrationStillPending =
                wasStarting || update?.phase == AdvertisementUpdatePhase.REGISTERING
            val error = if (registrationStillPending) {
                try {
                    unregisterRegistrationOnce(
                        session,
                        session.generation,
                        session.listener
                    )
                    null
                } catch (error: Throwable) {
                    error
                }
            } else {
                unregisterRegistrationWithRetry(
                    session,
                    session.generation,
                    session.listener
                )
            }
            if (error != null) {
                // A start or replacement registration still owes a readiness
                // callback. Keep the stop pending so a late acknowledgement can
                // retry cleanup with the now-confirmed native registration.
                if (!registrationStillPending) {
                    scheduleAdvertisementStopRetry(session, error)
                }
            }
        }
    }

    // ── DNS-SD commands ───────────────────────────────────────────────────────

    private fun protoSuffix(protocol: String): String =
        if (protocol.equals("tcp", ignoreCase = true)) "_tcp" else "_udp"

    /**
     * Format a resolved host + port as a socket-address string. Numeric IPv6
     * scopes are preserved.
     */
    private fun formatSocketAddr(host: InetAddress, port: Int): String {
        val literal = host.hostAddress?.substringBefore('%') ?: return ""
        return if (host is Inet6Address) {
            val scope = host.scopeId.takeIf { it > 0 }?.let { "%$it" } ?: ""
            "[$literal$scope]:$port"
        } else {
            "$literal:$port"
        }
    }

    private fun finishBrowseStop(session: DnsSdBrowseSession, error: String? = null) {
        removeBrowseSession(session)
        val waiters = session.pendingStops.toList()
        session.pendingStops.clear()
        for (waiter in waiters) {
            if (error == null) waiter.resolve() else waiter.reject(error)
        }
    }

    /** Retry an unaccepted active browse stop while preserving its first Invoke. */
    private fun scheduleBrowseStopRetry(session: DnsSdBrowseSession, error: Throwable) {
        if (
            session.stopRetryScheduled ||
            dnsSdBrowseMap[session.id] !== session ||
            session.nativeTerminal ||
            session.state != NativeSessionState.CLOSED
        ) return
        session.stopRetryScheduled = true
        val delayMillis = session.stopRetryDelayMillis
        session.stopRetryDelayMillis =
            (delayMillis * 2).coerceAtMost(MAX_STOP_RETRY_DELAY_MILLIS)
        Log.w(
            "iroh-http-dnssd",
            "browse ${session.id} stop dispatch failed; retrying in " +
                "${delayMillis}ms: ${error.message}"
        )
        lifecycleHandler.postDelayed({
            synchronized(session) {
                session.stopRetryScheduled = false
                if (
                    dnsSdBrowseMap[session.id] !== session ||
                    session.nativeTerminal ||
                    session.state != NativeSessionState.CLOSED ||
                    session.stopDispatchAccepted
                ) return@synchronized
                if (session.readinessPending) {
                    session.stopDeferredUntilReady = true
                    return@synchronized
                }

                val retryError = dispatchBrowseStopWithRetry(session)
                if (retryError == null) {
                    session.stopRetryDelayMillis = INITIAL_STOP_RETRY_DELAY_MILLIS
                } else {
                    scheduleBrowseStopRetry(session, retryError)
                }
            }
        }, delayMillis)
    }

    private fun rejectPendingBrowseStart(
        session: DnsSdBrowseSession,
        cleanupError: String? = null
    ) {
        val startError = session.pendingStartFailure ?: return
        session.pendingStartFailure = null
        session.startInvoke.reject(
            cleanupError?.let { "$startError; native cleanup failed: $it" } ?: startError
        )
    }

    private fun dispatchBrowseStopWithRetry(session: DnsSdBrowseSession): Throwable? {
        if (session.stopDispatchAccepted || session.nativeTerminal) return null
        var lastError: Throwable? = null
        repeat(MAX_BROWSE_STOP_DISPATCH_ATTEMPTS) {
            try {
                session.manager.stopServiceDiscovery(session.listener)
                session.stopDispatchAccepted = true
                return null
            } catch (error: Throwable) {
                lastError = error
            }
        }
        return lastError
    }

    @Command
    fun browse_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(DnsSdBrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"
        val startInvoke = InvokeOnce(invoke)
        val multicastLease = try {
            acquireNsdMulticastLease()
        } catch (error: SecurityException) {
            return invoke.reject(
                "DNS-SD browse requires CHANGE_WIFI_MULTICAST_STATE: ${error.message}"
            )
        } catch (error: Throwable) {
            return invoke.reject("Failed to prepare DNS-SD multicast: ${error.message}")
        }

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-dnssd", "browse $browseId start failed: $errorCode")
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    val message = "Failed to start DNS-SD browse: error $errorCode"
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    val retainedFailedStart = session.pendingStartFailure != null
                    val stopPending = session.pendingStops.isNotEmpty()
                    session.readinessPending = false
                    session.nativeTerminal = true
                    session.multicastLease?.release()
                    session.stopDispatchAccepted = false
                    session.state = NativeSessionState.FAILED
                    session.terminalError = message
                    retireResolveRequests(session)
                    rejectPendingBrowseStart(session)
                    if (failedBeforeReady) {
                        removeBrowseSession(session)
                        session.startInvoke.reject(message)
                        // Start failure is already terminal. In particular,
                        // API 21 removes the listener after this callback; a
                        // nested stop would target a failed discovery key.
                    }
                    if (stopPending) {
                        session.state = NativeSessionState.CLOSED
                        finishBrowseStop(session)
                    } else if (retainedFailedStart) {
                        removeBrowseSession(session)
                    }
                }
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    val retainedFailedStart = session.pendingStartFailure != null
                    val stopPending = session.pendingStops.isNotEmpty()
                    session.readinessPending = false
                    session.nativeTerminal = true
                    session.multicastLease?.release()
                    session.stopDispatchAccepted = false
                    session.state = NativeSessionState.FAILED
                    session.terminalError = "DNS-SD browse stop failed: error $errorCode"
                    retireResolveRequests(session)
                    rejectPendingBrowseStart(session, session.terminalError)
                    if (failedBeforeReady) {
                        removeBrowseSession(session)
                        session.startInvoke.reject(session.terminalError!!)
                    }
                    if (stopPending) {
                        finishBrowseStop(session, session.terminalError)
                    } else if (retainedFailedStart) {
                        removeBrowseSession(session)
                    }
                }
            }
            override fun onDiscoveryStarted(serviceType: String) {
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    session.readinessPending = false
                    if (session.state == NativeSessionState.CLOSED) {
                        session.stopDeferredUntilReady = false
                        val error = dispatchBrowseStopWithRetry(session)
                        if (error != null) {
                            scheduleBrowseStopRetry(session, error)
                        }
                        return
                    }
                    if (session.state != NativeSessionState.STARTING) return
                    session.state = NativeSessionState.ACTIVE
                    val ret = JSObject()
                    ret.put("browseId", browseId)
                    session.startInvoke.resolve(ret)
                }
            }
            override fun onDiscoveryStopped(serviceType: String) {
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    val stopPending = session.pendingStops.isNotEmpty()
                    val stoppedBeforeReady = session.state == NativeSessionState.STARTING
                    val retainedFailedStart = session.pendingStartFailure != null
                    session.readinessPending = false
                    session.nativeTerminal = true
                    session.multicastLease?.release()
                    session.stopDispatchAccepted = false
                    if (stoppedBeforeReady) {
                        session.state = NativeSessionState.CLOSED
                        session.startInvoke.reject("DNS-SD browse stopped before becoming ready")
                    } else if (session.state == NativeSessionState.ACTIVE) {
                        session.state = NativeSessionState.CLOSED
                    }
                    retireResolveRequests(session)
                    rejectPendingBrowseStart(session)
                    if (stopPending) {
                        finishBrowseStop(session)
                    } else if (stoppedBeforeReady || retainedFailedStart) {
                        removeBrowseSession(session)
                    }
                }
            }

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                val session = dnsSdBrowseMap[browseId] ?: return
                val instanceName = serviceInfo.serviceName
                val generation = synchronized(session) {
                    if (
                        dnsSdBrowseMap[browseId] !== session ||
                        session.state != NativeSessionState.ACTIVE
                    ) return
                    // One generation represents one continuous presence epoch.
                    // Repeated announcements while the instance remains present
                    // must all resolve and surface; loss removes the epoch so a
                    // late callback cannot revive a later reappearance.
                    session.presenceGenerations[instanceName] ?: run {
                        val current = session.nextPresenceGeneration++
                        session.presenceGenerations[instanceName] = current
                        current
                    }
                }
                enqueueResolve(
                    ResolveOwner(session, instanceName, generation),
                    session.manager,
                    serviceInfo,
                    object : NsdManager.ResolveListener {
                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        val name = instanceName

                        val txt = JSObject()
                        resolved.attributes?.forEach { (k, v) ->
                            txt.put(k, if (v != null) String(v, StandardCharsets.UTF_8) else "")
                        }

                        val addrs = JSONArray()
                        val host = resolved.host
                        val hostAddr = host?.hostAddress
                        // #346: `addrs` must hold well-formed socket addresses.
                        // A bare, port-less host (the resolved A-record IP) is
                        // undialable and fails `parse_direct_addrs` for the whole
                        // list, so pair it with the SRV port. Advertisers whose
                        // SRV port is not the QUIC port (iOS) additionally publish
                        // a dialable `address` TXT that the consumer prefers.
                        // #350 F12: a placeholder SRV port (<=1, e.g. Android's
                        // own advertiser publishes port 1) is never a real QUIC
                        // socket, so never surface `host:1`; the consumer relies
                        // on the `address` TXT for those advertisers.
                        if (host != null && resolved.port > 1) {
                            formatSocketAddr(host, resolved.port)
                                .takeIf { it.isNotEmpty() }
                                ?.let { addrs.put(it) }
                        }

                        synchronized(session) {
                            if (
                                dnsSdBrowseMap[browseId] !== session ||
                                session.state != NativeSessionState.ACTIVE ||
                                session.presenceGenerations[name] != generation
                            ) return
                            session.knownInstances.add(name)

                            val record = JSObject()
                            record.put("isActive", true)
                            record.put("serviceType", session.serviceType)
                            record.put("instanceName", name)
                            record.put("host", hostAddr ?: JSONObject.NULL)
                            record.put("port", resolved.port)
                            record.put("addrs", addrs)
                            record.put("txt", txt)
                            session.pendingRecords.add(record)
                        }
                    }

                    override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
                    }
                )
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                val session = dnsSdBrowseMap[browseId] ?: return
                val name = serviceInfo.serviceName
                var retiredOwner: ResolveOwner? = null
                synchronized(session) {
                    if (
                        dnsSdBrowseMap[browseId] !== session ||
                        session.state != NativeSessionState.ACTIVE
                    ) return
                    session.presenceGenerations.remove(name)?.let { generation ->
                        retiredOwner = ResolveOwner(session, name, generation)
                    }
                    if (session.knownInstances.remove(name)) {
                        val record = JSObject()
                        record.put("isActive", false)
                        record.put("serviceType", session.serviceType)
                        record.put("instanceName", name)
                        record.put("host", JSONObject.NULL)
                        record.put("port", 0)
                        record.put("addrs", JSONArray())
                        record.put("txt", JSObject())
                        session.pendingRecords.add(record)
                    }
                }
                retiredOwner?.let { retireResolveRequest(it) }
            }
        }

        val session = DnsSdBrowseSession(
            browseId,
            manager,
            listener,
            serviceType,
            startInvoke,
            multicastLease
        )
        val dispatchError = synchronized(resolveQueue) {
            resolverTerminalError?.let { terminalError ->
                session.state = NativeSessionState.FAILED
                session.terminalError = terminalError
                session.readinessPending = false
                session.startInvoke.reject(terminalError)
                session.multicastLease?.release()
                return
            }
            dnsSdBrowseMap[browseId] = session
            try {
                manager.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener)
                null
            } catch (error: Throwable) {
                error
            }
        }

        dispatchError?.let { e ->
            synchronized(session) {
                val message = "Discovery failed: ${e.message}"
                if (e is IllegalArgumentException) {
                    removeBrowseSession(session)
                    session.state = NativeSessionState.FAILED
                    session.terminalError = message
                    session.startInvoke.reject(message)
                    return
                }
                session.state = NativeSessionState.CLOSED
                session.terminalError = message
                session.pendingStartFailure = message
                retireResolveRequests(session)
                val cleanupError = dispatchBrowseStopWithRetry(session)
                if (cleanupError != null) {
                    session.stopDeferredUntilReady = true
                    session.terminalError = "$message; cleanup: ${cleanupError.message}"
                }
            }
        }
    }

    @Command
    fun browse_poll(invoke: Invoke) {
        val args = invoke.parseArgs(BrowsePollArgs::class.java)
        val session = dnsSdBrowseMap[args.browseId]
        val ret = JSObject()
        if (session == null) {
            ret.put("status", NativeSessionState.CLOSED.pollValue)
            ret.put("records", JSONArray())
        } else {
            synchronized(session) {
                val records = session.pendingRecords.toList()
                session.pendingRecords.clear()
                val arr = JSONArray()
                records.forEach { arr.put(it) }
                ret.put("status", session.state.pollValue)
                ret.put("records", arr)
                session.terminalError?.let { ret.put("error", it) }
                if (
                    session.nativeTerminal &&
                    (session.state == NativeSessionState.CLOSED ||
                        session.state == NativeSessionState.FAILED)
                ) {
                    if (dnsSdBrowseMap[args.browseId] === session) {
                        removeBrowseSession(session)
                    }
                }
            }
        }
        invoke.resolve(ret)
    }

    @Command
    fun browse_stop(invoke: Invoke) {
        val args = invoke.parseArgs(BrowseStopArgs::class.java)
        val completion = InvokeOnce(invoke)
        val session = dnsSdBrowseMap[args.browseId] ?: return completion.resolve()
        synchronized(session) {
            if (dnsSdBrowseMap[args.browseId] !== session) return completion.resolve()
            session.pendingStops.add(completion)
            if (session.nativeTerminal) {
                finishBrowseStop(session)
                return
            }
            if (session.state != NativeSessionState.CLOSED) {
                val wasStarting = session.state == NativeSessionState.STARTING
                session.state = NativeSessionState.CLOSED
                session.presenceGenerations.clear()
                retireResolveRequests(session)
                if (wasStarting) {
                    session.startInvoke.reject("DNS-SD browse stopped before becoming ready")
                }
            }

            if (
                session.stopDispatchAccepted ||
                session.stopDeferredUntilReady ||
                session.stopRetryScheduled
            ) return
            if (session.readinessPending) {
                try {
                    session.manager.stopServiceDiscovery(session.listener)
                    session.stopDispatchAccepted = true
                } catch (_: Throwable) {
                    session.stopDeferredUntilReady = true
                }
                return
            }

            val error = dispatchBrowseStopWithRetry(session)
            if (error != null) {
                scheduleBrowseStopRetry(session, error)
            }
        }
    }

    @Command
    fun advertise_start(invoke: Invoke) {
        val args = invoke.parseArgs(DnsSdAdvertiseStartArgs::class.java)
        if (args.addrs.isNotEmpty()) {
            return invoke.reject(
                "Android generic DNS-SD advertising does not support explicit addrs"
            )
        }
        if (args.port !in 1..65535) {
            return invoke.reject("DNS-SD port must be between 1 and 65535")
        }
        val instanceNameBytes = args.instanceName.toByteArray(StandardCharsets.UTF_8)
        if (instanceNameBytes.isEmpty() || instanceNameBytes.size > 63) {
            return invoke.reject("DNS-SD instanceName must contain 1...63 UTF-8 bytes")
        }
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val advertiseId = nextAdvertiseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"

        val info = try {
            NsdServiceInfo().apply {
                serviceName = args.instanceName
                this.serviceType = serviceType
                setPort(args.port)
                args.txt.forEach { (k, v) -> setAttribute(k, v) }
            }
        } catch (e: Throwable) {
            return invoke.reject("Failed to encode DNS-SD record: ${e.message}")
        }
        val multicastLease = try {
            acquireNsdMulticastLease()
        } catch (error: SecurityException) {
            return invoke.reject(
                "DNS-SD advertising requires CHANGE_WIFI_MULTICAST_STATE: ${error.message}"
            )
        } catch (error: Throwable) {
            return invoke.reject("Failed to prepare DNS-SD multicast: ${error.message}")
        }
        startAdvertisement(manager, advertiseId, info, multicastLease, invoke)
    }

    @Command
    fun advertise_update(invoke: Invoke) {
        val args = invoke.parseArgs(DnsSdAdvertiseUpdateArgs::class.java)
        if (args.addrs.isNotEmpty()) {
            return invoke.reject(
                "Android generic DNS-SD advertising does not support explicit addrs"
            )
        }
        if (args.port !in 1..65535) {
            return invoke.reject("DNS-SD port must be between 1 and 65535")
        }
        val session = advertiseMap[args.advertiseId]
            ?: return invoke.reject("DNS-SD advertisement is closed")
        val identity = synchronized(session) {
            Pair(session.info.serviceName, session.info.serviceType)
        }
        val info = try {
            NsdServiceInfo().apply {
                serviceName = identity.first
                serviceType = identity.second
                setPort(args.port)
                args.txt.forEach { (key, value) -> setAttribute(key, value) }
            }
        } catch (error: Throwable) {
            return invoke.reject("Failed to encode DNS-SD record: ${error.message}")
        }
        updateAdvertisement(args.advertiseId, info, invoke)
    }

    @Command
    fun advertise_stop(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseStopArgs::class.java)
        stopAdvertisement(args.advertiseId, invoke)
    }
}
