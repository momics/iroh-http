package com.iroh.http

import android.app.Activity
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
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
import java.util.LinkedHashSet
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

@InvokeArg
class BrowseStartArgs {
    lateinit var serviceName: String
}

@InvokeArg
class BrowsePollArgs {
    var browseId: Long = 0
}

@InvokeArg
class BrowseStopArgs {
    var browseId: Long = 0
}

@InvokeArg
class AdvertiseStartArgs {
    lateinit var serviceName: String
    lateinit var pk: String
    var relay: String? = null

    // Structured direct `ip:port` candidates. Native DNS-SD transports these
    // as one comma-separated TXT value while preserving every candidate's
    // authoritative port.
    var addresses: List<String> = emptyList()
}

@InvokeArg
class AdvertiseUpdateArgs {
    var advertiseId: Long = 0
    var relay: String? = null
    var addresses: List<String> = emptyList()
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
class DnsSdBrowseStartArgs {
    lateinit var serviceName: String
    var protocol: String = "udp"
}

@TauriPlugin
class IrohHttpPlugin(private val activity: Activity) : Plugin(activity) {

    private companion object {
        const val MAX_ADDRESS_TXT_VALUE_BYTES = 247
    }

    private val nextBrowseId = AtomicLong(1)
    private val nextAdvertiseId = AtomicLong(1)

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

    private class BrowseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.DiscoveryListener,
        val startInvoke: InvokeOnce,
        var state: NativeSessionState = NativeSessionState.STARTING,
        var terminalError: String? = null,
        val pendingEvents: MutableList<JSObject> = mutableListOf(),
        // fullServiceName → snapshot. Keyed by a signature of (nodeId + sorted
        // dialable addrs) so a peer that rebinds to a new address under the SAME
        // nodeId (the iOS foreground-restart-rebind case, #336) re-emits instead
        // of being suppressed by a nodeId-only dedup (#350 review M2).
        val knownNodes: MutableMap<String, PeerSnapshot> = mutableMapOf(),
        // Service-instance presence generation. Every `onServiceFound`
        // advances it; `onServiceLost` removes it. A queued resolve may only
        // commit while its captured generation is still current.
        val presenceGenerations: MutableMap<String, Long> = mutableMapOf(),
        var nextPresenceGeneration: Long = 1
    )

    private data class PeerSnapshot(val nodeId: String, val signature: String)

    private sealed class AdvertisementKind {
        data class Peer(val serviceName: String, val pk: String) : AdvertisementKind()
        object Generic : AdvertisementKind()
    }

    private enum class AdvertisementUpdatePhase {
        UNREGISTERING,
        REGISTERING
    }

    private data class AdvertisementUpdate(
        val info: NsdServiceInfo,
        val invoke: InvokeOnce,
        var phase: AdvertisementUpdatePhase = AdvertisementUpdatePhase.UNREGISTERING
    )

    private class AdvertiseSession(
        val id: Long,
        val manager: NsdManager,
        val kind: AdvertisementKind,
        val startInvoke: InvokeOnce,
        var state: NativeSessionState = NativeSessionState.STARTING,
        var terminalError: String? = null,
        var generation: Long = 1,
        var pendingUpdate: AdvertisementUpdate? = null,
        // One unregister request is allowed per generation. AOSP keeps the
        // listener mapped while unregisterService() dispatches, then retires
        // it at the terminal callback boundary (after the callback on API 21,
        // before it on current Android). Registration failure retires it at
        // the analogous callback boundary too. Either way, a terminally
        // retired listener must never be retried or reused.
        val retiredRegistrationGenerations: MutableSet<Long> = mutableSetOf()
    ) {
        lateinit var listener: NsdManager.RegistrationListener
        lateinit var info: NsdServiceInfo
    }

    /** A generic DNS-SD browse session, carrying full records rather than peers. */
    private class DnsSdBrowseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.DiscoveryListener,
        val serviceType: String,
        val startInvoke: InvokeOnce,
        var state: NativeSessionState = NativeSessionState.STARTING,
        var terminalError: String? = null,
        val pendingRecords: MutableList<JSObject> = mutableListOf(),
        // Instances that have produced at least one resolved record. This is
        // presence bookkeeping for removals, not record de-duplication: generic
        // browse exposes every platform announcement, including identical ones.
        val knownInstances: MutableSet<String> = mutableSetOf(),
        val presenceGenerations: MutableMap<String, Long> = mutableMapOf(),
        var nextPresenceGeneration: Long = 1
    )

    /** Provenance for one serialized legacy NsdManager resolve request. */
    private sealed class ResolveOwner {
        data class Peer(
            val session: BrowseSession,
            val instanceName: String,
            val presenceGeneration: Long
        ) : ResolveOwner()

        data class Generic(
            val session: DnsSdBrowseSession,
            val instanceName: String,
            val presenceGeneration: Long
        ) : ResolveOwner()
    }

    private val browseMap = ConcurrentHashMap<Long, BrowseSession>()
    private val advertiseMap = ConcurrentHashMap<Long, AdvertiseSession>()
    private val dnsSdBrowseMap = ConcurrentHashMap<Long, DnsSdBrowseSession>()

    private fun nsd(): NsdManager? =
        activity.getSystemService(Context.NSD_SERVICE) as? NsdManager

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
    // whenever several peers/services appear together. Both the peer
    // (`browse_peers_start`) and generic (`browse_start`) browse paths share
    // this single queue so resolves across sessions are serialized too.
    private data class ResolveRequest(
        val owner: ResolveOwner,
        val manager: NsdManager,
        val serviceInfo: NsdServiceInfo,
        val listener: NsdManager.ResolveListener
    )

    private val resolveQueue = java.util.ArrayDeque<ResolveRequest>()
    private var resolveInProgress = false

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

    private fun isCurrentResolveOwner(owner: ResolveOwner): Boolean = when (owner) {
        is ResolveOwner.Peer -> synchronized(owner.session) {
            browseMap[owner.session.id] === owner.session &&
                owner.session.state == NativeSessionState.ACTIVE &&
                owner.session.presenceGenerations[owner.instanceName] ==
                owner.presenceGeneration
        }
        is ResolveOwner.Generic -> synchronized(owner.session) {
            dnsSdBrowseMap[owner.session.id] === owner.session &&
                owner.session.state == NativeSessionState.ACTIVE &&
                owner.session.presenceGenerations[owner.instanceName] ==
                owner.presenceGeneration
        }
    }

    /** Drop queued work for a retired session without disturbing another owner. */
    private fun retireResolveRequests(session: Any) {
        synchronized(resolveQueue) {
            val iterator = resolveQueue.iterator()
            while (iterator.hasNext()) {
                val belongsToSession = when (val owner = iterator.next().owner) {
                    is ResolveOwner.Peer -> owner.session === session
                    is ResolveOwner.Generic -> owner.session === session
                }
                if (belongsToSession) iterator.remove()
            }
        }
    }

    private fun drainResolveQueue() {
        var next: ResolveRequest? = null
        while (next == null) {
            val candidate = synchronized(resolveQueue) {
                resolveQueue.pollFirst().also { polled ->
                    if (polled == null) resolveInProgress = false
                }
            }
            if (candidate == null) return
            if (isCurrentResolveOwner(candidate.owner)) next = candidate
        }
        val request = next
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
        val completed = AtomicBoolean(false)
        fun finish(callback: () -> Unit) {
            if (!completed.compareAndSet(false, true)) return
            try { callback() } finally { drainResolveQueue() }
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
        try {
            manager.resolveService(serviceInfo, wrapped)
        } catch (e: Throwable) {
            Log.w(
                "iroh-http-mdns",
                "resolve threw for ${serviceInfo.serviceName}: ${e.message}"
            )
            finish { listener.onResolveFailed(serviceInfo, -1) }
        }
    }

    /**
     * Validate that a string is a canonical iroh endpoint id: a 32-byte Ed25519
     * public key encoded as lowercase RFC 4648 base32 without padding, i.e.
     * exactly 52 characters drawn from the `a-z` / `2-7` alphabet.
     *
     * Used to safely accept the DNS-SD instance name as the node-id when a
     * peer's advertisement carries no `pk` attribute. Every current
     * `advertise_peer` implementation (desktop's `mdns-sd`-backed advertiser
     * included) sets `pk`, so this is a defensive fallback for
     * advertisements from older or third-party peers rather than the normal
     * path. The advertise side truncates instance names to 63 chars, which
     * does not truncate a 52-char id, so the recovered id is always
     * complete.
     */
    private fun isValidEndpointId(s: String): Boolean {
        if (s.length != 52) return false
        return s.all { c -> c in 'a'..'z' || c in '2'..'7' }
    }

    /**
     * Return a trimmed, dialable numeric socket literal. Host names and
     * placeholder ports are rejected. IPv6 must be bracketed; link-local IPv6
     * additionally requires a numeric, non-zero scope id.
     */
    private fun validatedSocketLiteral(raw: String): String? {
        val candidate = raw.trim()
        if (candidate.isEmpty()) return null

        val host: String
        val portText: String
        val bracketedV6: Boolean
        if (candidate.startsWith("[")) {
            val close = candidate.lastIndexOf(']')
            if (close <= 1 || close + 1 >= candidate.length || candidate[close + 1] != ':') {
                return null
            }
            host = candidate.substring(1, close)
            portText = candidate.substring(close + 2)
            bracketedV6 = true
        } else {
            val separator = candidate.lastIndexOf(':')
            if (separator <= 0 || candidate.indexOf(':') != separator) return null
            host = candidate.substring(0, separator)
            portText = candidate.substring(separator + 1)
            bracketedV6 = false
        }

        if (portText.isEmpty() || portText.any { it !in '0'..'9' }) return null
        val port = portText.toIntOrNull() ?: return null
        if (port !in 2..65535) return null

        val parsed: InetAddress
        var hasNumericScope = false
        if (bracketedV6) {
            val scopeParts = host.split('%')
            if (
                scopeParts.size > 2 || scopeParts[0].isEmpty() ||
                !scopeParts[0].contains(':')
            ) return null
            if (scopeParts.size == 2) {
                val scope = scopeParts[1]
                val scopeId = scope.toLongOrNull()
                if (
                    scope.isEmpty() || scope.any { it !in '0'..'9' } ||
                    scopeId == null || scopeId !in 1..0xffff_ffffL
                ) {
                    return null
                }
                hasNumericScope = true
            }
            parsed = try {
                InetAddress.getByName(scopeParts[0])
            } catch (_: Throwable) {
                return null
            }
            if (parsed !is Inet6Address) return null
        } else {
            val octets = host.split('.')
            if (
                octets.size != 4 ||
                octets.any { part ->
                    part.isEmpty() || part.any { it !in '0'..'9' } ||
                        (part.length > 1 && part.startsWith('0')) ||
                        part.toIntOrNull()?.let { it !in 0..255 } != false
                }
            ) {
                return null
            }
            parsed = try {
                InetAddress.getByName(host)
            } catch (_: Throwable) {
                return null
            }
            if (parsed is Inet6Address) return null
        }

        if (
            parsed.isAnyLocalAddress || parsed.isLoopbackAddress ||
            parsed.isMulticastAddress ||
            (parsed.isLinkLocalAddress && !hasNumericScope)
        ) {
            return null
        }
        return candidate
    }

    /** Validate/de-duplicate candidates without letting one bad member poison the rest. */
    private fun validatedSocketLiterals(candidates: Iterable<String>): List<String> {
        val result = LinkedHashSet<String>()
        for (candidate in candidates) validatedSocketLiteral(candidate)?.let(result::add)
        return result.toList()
    }

    /**
     * Select a stable subset of complete candidates for one DNS-SD TXT value.
     * `address=` consumes eight of the 255 bytes allowed for a TXT entry,
     * leaving at most 247 UTF-8 bytes for its comma-separated value. A member
     * that does not fit is skipped without hiding a later shorter member.
     */
    private fun stableFittingAddressTxtSubset(candidates: Iterable<String>): String? {
        val fitted = mutableListOf<String>()
        var usedBytes = 0
        for (candidate in validatedSocketLiterals(candidates)) {
            val candidateBytes = candidate.toByteArray(StandardCharsets.UTF_8).size
            val separatorBytes = if (fitted.isEmpty()) 0 else 1
            if (
                usedBytes + separatorBytes + candidateBytes >
                MAX_ADDRESS_TXT_VALUE_BYTES
            ) continue
            fitted.add(candidate)
            usedBytes += separatorBytes + candidateBytes
        }
        return fitted.takeIf { it.isNotEmpty() }?.joinToString(",")
    }

    private fun peerAddresses(resolved: NsdServiceInfo): List<String> {
        val addresses = LinkedHashSet<String>()
        val advertised = resolved.attributes["address"]
            ?.let { String(it, StandardCharsets.UTF_8) }
            ?.split(',')
            ?: emptyList()
        val direct = validatedSocketLiterals(advertised)
        addresses.addAll(direct)

        // An invalid/non-dialable `address` TXT is equivalent to no direct TXT:
        // it must not suppress a valid SRV host:port fallback.
        if (direct.isEmpty() && resolved.port > 1) {
            resolved.host?.let { host ->
                validatedSocketLiteral(formatSocketAddr(host, resolved.port))?.let(addresses::add)
            }
        }
        resolved.attributes["relay"]
            ?.let { String(it, StandardCharsets.UTF_8).trim() }
            ?.takeIf { it.isNotEmpty() }
            ?.let(addresses::add)
        return addresses.toList()
    }

    // ── Browse ────────────────────────────────────────────────────────────────

    @Command
    fun browse_peers_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(BrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}._udp"
        val startInvoke = InvokeOnce(invoke)

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-mdns", "browse $browseId start failed: $errorCode")
                val session = browseMap[browseId] ?: return
                synchronized(session) {
                    if (browseMap[browseId] !== session) return
                    val message = "Failed to start peer browse: error $errorCode"
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    session.state = NativeSessionState.FAILED
                    session.terminalError = message
                    retireResolveRequests(session)
                    if (failedBeforeReady) {
                        // No handle escaped from a rejected start, so retaining
                        // this entry would leak an unreachable session.
                        browseMap.remove(browseId)
                        session.startInvoke.reject(message)
                        // Do not call stopServiceDiscovery here. AOSP treats a
                        // start failure as terminal; API 21 retires the listener
                        // only after this callback returns, so stopping from
                        // inside it can enqueue a second operation on a failed
                        // discovery key.
                    }
                }
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                val session = browseMap[browseId] ?: return
                synchronized(session) {
                    if (browseMap[browseId] !== session) return
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    session.state = NativeSessionState.FAILED
                    session.terminalError = "Peer browse stop failed: error $errorCode"
                    retireResolveRequests(session)
                    if (failedBeforeReady) {
                        browseMap.remove(browseId)
                        session.startInvoke.reject(session.terminalError!!)
                    }
                }
            }
            override fun onDiscoveryStarted(serviceType: String) {
                val session = browseMap[browseId] ?: return
                synchronized(session) {
                    if (
                        browseMap[browseId] !== session ||
                        session.state != NativeSessionState.STARTING
                    ) return
                    session.state = NativeSessionState.ACTIVE
                    val ret = JSObject()
                    ret.put("browseId", browseId)
                    session.startInvoke.resolve(ret)
                }
            }
            override fun onDiscoveryStopped(serviceType: String) {
                val session = browseMap[browseId] ?: return
                synchronized(session) {
                    if (browseMap[browseId] !== session) return
                    if (session.state == NativeSessionState.STARTING) {
                        session.state = NativeSessionState.CLOSED
                        browseMap.remove(browseId)
                        session.startInvoke.reject("Peer browse stopped before becoming ready")
                    } else if (session.state == NativeSessionState.ACTIVE) {
                        // Retain terminal state until exactly one poll observes it.
                        session.state = NativeSessionState.CLOSED
                    }
                    retireResolveRequests(session)
                }
            }

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                val session = browseMap[browseId] ?: return
                val instanceName = serviceInfo.serviceName
                val generation = synchronized(session) {
                    if (
                        browseMap[browseId] !== session ||
                        session.state != NativeSessionState.ACTIVE
                    ) return
                    val current = session.nextPresenceGeneration++
                    session.presenceGenerations[instanceName] = current
                    current
                }
                enqueueResolve(
                    ResolveOwner.Peer(session, instanceName, generation),
                    session.manager,
                    serviceInfo,
                    object : NsdManager.ResolveListener {
                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        // Prefer the `pk` attribute (set by every current
                        // advertiser, mobile and desktop alike); fall back to
                        // the DNS-SD instance name — which desktop's
                        // `mdns-sd`-backed advertiser publishes as the base32
                        // endpoint id too — for advertisements from older or
                        // third-party peers that carry no `pk` attribute.
                        val pkAttr = resolved.attributes["pk"]
                            ?.let { String(it, StandardCharsets.UTF_8) }
                        val nodeId = if (!pkAttr.isNullOrEmpty()) {
                            pkAttr
                        } else {
                            val name = instanceName
                            if (isValidEndpointId(name)) name else return
                        }

                        val key = instanceName
                        val resolvedAddrs = peerAddresses(resolved)

                        // #350 review M2: dedup on (nodeId + sorted addrs) so a
                        // rebind under the same nodeId re-emits with the new
                        // address instead of being suppressed.
                        val sortedAddrs = resolvedAddrs.sorted()
                        val signature = buildString {
                            append(nodeId.length).append(':').append(nodeId)
                            for (address in sortedAddrs) {
                                append(address.length).append(':').append(address)
                            }
                        }
                        synchronized(session) {
                            if (
                                browseMap[browseId] !== session ||
                                session.state != NativeSessionState.ACTIVE ||
                                session.presenceGenerations[key] != generation
                            ) return
                            if (session.knownNodes[key]?.signature == signature) return
                            session.knownNodes[key] = PeerSnapshot(nodeId, signature)

                            val addrs = JSONArray()
                            resolvedAddrs.forEach { addrs.put(it) }
                            val event = JSObject()
                            event.put("type", "discovered")
                            event.put("instanceName", key)
                            event.put("nodeId", nodeId)
                            event.put("addrs", addrs)
                            session.pendingEvents.add(event)
                        }
                    }

                    override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
                    }
                )
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                val session = browseMap[browseId] ?: return
                val instanceName = serviceInfo.serviceName
                synchronized(session) {
                    if (
                        browseMap[browseId] !== session ||
                        session.state != NativeSessionState.ACTIVE
                    ) return
                    // Invalidate queued resolves even when no resolved snapshot
                    // has been emitted yet (found → lost → late resolve).
                    session.presenceGenerations.remove(instanceName)
                    val snapshot = session.knownNodes.remove(instanceName) ?: return
                    val event = JSObject()
                    event.put("type", "expired")
                    event.put("instanceName", instanceName)
                    event.put("nodeId", snapshot.nodeId)
                    event.put("addrs", JSONArray())
                    session.pendingEvents.add(event)
                }
            }
        }

        val session = BrowseSession(browseId, manager, listener, startInvoke)
        browseMap[browseId] = session

        try {
            manager.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Throwable) {
            synchronized(session) {
                if (browseMap[browseId] === session) browseMap.remove(browseId)
                session.state = NativeSessionState.FAILED
                session.terminalError = e.message
                retireResolveRequests(session)
                session.startInvoke.reject("Discovery failed: ${e.message}")
            }
        }
    }

    @Command
    fun browse_peers_poll(invoke: Invoke) {
        val args = invoke.parseArgs(BrowsePollArgs::class.java)
        val session = browseMap[args.browseId]
        val ret = JSObject()
        if (session == null) {
            ret.put("status", NativeSessionState.CLOSED.pollValue)
            ret.put("events", JSONArray())
        } else {
            synchronized(session) {
                val events = session.pendingEvents.toList()
                session.pendingEvents.clear()
                val arr = JSONArray()
                events.forEach { arr.put(it) }
                ret.put("status", session.state.pollValue)
                ret.put("events", arr)
                session.terminalError?.let { ret.put("error", it) }
                if (
                    session.state == NativeSessionState.CLOSED ||
                    session.state == NativeSessionState.FAILED
                ) {
                    if (browseMap[args.browseId] === session) browseMap.remove(args.browseId)
                }
            }
        }
        invoke.resolve(ret)
    }

    @Command
    fun browse_peers_stop(invoke: Invoke) {
        val args = invoke.parseArgs(BrowseStopArgs::class.java)
        val session = browseMap.remove(args.browseId)
        if (session != null) {
            synchronized(session) {
                val wasStarting = session.state == NativeSessionState.STARTING
                session.state = NativeSessionState.CLOSED
                session.presenceGenerations.clear()
                retireResolveRequests(session)
                if (wasStarting) {
                    session.startInvoke.reject("Peer browse stopped before becoming ready")
                }
            }
            try { session.manager.stopServiceDiscovery(session.listener) } catch (_: Throwable) {}
        }
        invoke.resolve()
    }

    // ── Advertise ─────────────────────────────────────────────────────────────

    private fun peerServiceInfo(
        serviceName: String,
        pk: String,
        relay: String?,
        addresses: Iterable<String>
    ): NsdServiceInfo = NsdServiceInfo().apply {
        this.serviceName = pk.take(63)
        this.serviceType = "_${serviceName}._udp"
        setPort(1) // SRV placeholder; complete QUIC candidates live in TXT.
        setAttribute("pk", pk)
        relay?.trim()?.takeIf { it.isNotEmpty() }?.let { setAttribute("relay", it) }
        stableFittingAddressTxtSubset(addresses)?.let { setAttribute("address", it) }
    }

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
        return true
    }

    private fun registrationListener(
        session: AdvertiseSession,
        generation: Long
    ): NsdManager.RegistrationListener = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
            synchronized(session) {
                if (
                    advertiseMap[session.id] !== session ||
                    session.state == NativeSessionState.CLOSED
                ) {
                    // A stop can win while Android is still completing an
                    // asynchronous register. Tear down that late registration
                    // instead of leaking a native service with no owner.
                    try {
                        unregisterRegistrationOnce(session, generation, this)
                    } catch (_: Throwable) {}
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
                if (
                    advertiseMap[session.id] !== session ||
                    session.generation != generation ||
                    session.listener !== this
                ) return
                val message = "DNS-SD registration failed: error $errorCode"
                session.terminalError = message
                val update = session.pendingUpdate
                if (
                    update != null &&
                    update.phase == AdvertisementUpdatePhase.REGISTERING
                ) {
                    session.pendingUpdate = null
                    session.state = NativeSessionState.FAILED
                    update.invoke.reject("Peer advertisement update failed: error $errorCode")
                } else if (session.state == NativeSessionState.STARTING) {
                    session.state = NativeSessionState.FAILED
                    advertiseMap.remove(session.id)
                    session.startInvoke.reject(message)
                } else if (session.state == NativeSessionState.ACTIVE) {
                    session.state = NativeSessionState.FAILED
                }
                Log.e("iroh-http-dnssd", "advertise ${session.id} failed: $errorCode")
            }
        }

        override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {
            synchronized(session) {
                session.retiredRegistrationGenerations.add(generation)
                if (
                    advertiseMap[session.id] !== session ||
                    session.generation != generation ||
                    session.listener !== this
                ) return
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
                        update.invoke.reject("Peer advertisement update failed: ${e.message}")
                    }
                } else {
                    val message = "DNS-SD registration stopped unexpectedly"
                    session.terminalError = message
                    if (session.state == NativeSessionState.STARTING) {
                        session.state = NativeSessionState.FAILED
                        advertiseMap.remove(session.id)
                        session.startInvoke.reject(message)
                    } else if (session.state == NativeSessionState.ACTIVE) {
                        session.state = NativeSessionState.FAILED
                    }
                }
            }
        }

        override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            synchronized(session) {
                session.retiredRegistrationGenerations.add(generation)
                if (
                    advertiseMap[session.id] !== session ||
                    session.state == NativeSessionState.CLOSED
                ) {
                    // AOSP has retired this listener mapping at the callback
                    // boundary. Retrying it would throw and can never clean
                    // up the native service.
                    return
                }
                if (
                    session.generation != generation ||
                    session.listener !== this
                ) return
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
                        "Peer advertisement update could not unregister old record: error $errorCode"
                    )
                } else {
                    session.state = NativeSessionState.FAILED
                    session.terminalError = "DNS-SD unregistration failed: error $errorCode"
                }
            }
        }
    }

    private fun startAdvertisement(
        manager: NsdManager,
        advertiseId: Long,
        kind: AdvertisementKind,
        info: NsdServiceInfo,
        invoke: Invoke
    ) {
        val session = AdvertiseSession(advertiseId, manager, kind, InvokeOnce(invoke))
        synchronized(session) {
            session.info = info
            session.listener = registrationListener(session, session.generation)
            advertiseMap[advertiseId] = session
            try {
                manager.registerService(info, NsdManager.PROTOCOL_DNS_SD, session.listener)
            } catch (e: Throwable) {
                advertiseMap.remove(advertiseId)
                session.state = NativeSessionState.FAILED
                session.terminalError = e.message
                // Validation failures happen before NsdManager installs the
                // listener; transport failures can happen after installation.
                // A failed cleanup stays retryable by a possible late callback.
                try {
                    unregisterRegistrationOnce(
                        session,
                        session.generation,
                        session.listener
                    )
                } catch (_: Throwable) {}
                session.startInvoke.reject("Registration failed: ${e.message}")
            }
        }
    }

    @Command
    fun advertise_peer_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(AdvertiseStartArgs::class.java)
        val advertiseId = nextAdvertiseId.getAndIncrement()
        val info = try {
            peerServiceInfo(args.serviceName, args.pk, args.relay, args.addresses)
        } catch (e: Throwable) {
            return invoke.reject("Failed to encode peer DNS-SD record: ${e.message}")
        }
        startAdvertisement(
            manager,
            advertiseId,
            AdvertisementKind.Peer(args.serviceName, args.pk),
            info,
            invoke
        )
    }

    @Command
    fun advertise_peer_update(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseUpdateArgs::class.java)
        val completion = InvokeOnce(invoke)
        val session = advertiseMap[args.advertiseId]
            ?: return completion.reject("Peer advertisement is closed")
        val peer = session.kind as? AdvertisementKind.Peer
            ?: return completion.reject("Advertisement is not an iroh peer advertisement")
        val info = try {
            peerServiceInfo(peer.serviceName, peer.pk, args.relay, args.addresses)
        } catch (e: Throwable) {
            return completion.reject("Failed to encode peer DNS-SD record: ${e.message}")
        }

        synchronized(session) {
            if (advertiseMap[args.advertiseId] !== session) {
                return completion.reject("Peer advertisement is closed")
            }
            if (session.state != NativeSessionState.ACTIVE) {
                val reason = session.terminalError ?: "advertisement is not active"
                return completion.reject("Peer advertisement update failed: $reason")
            }
            if (session.pendingUpdate != null) {
                return completion.reject("Peer advertisement update already in progress")
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
                completion.reject("Peer advertisement update failed: ${e.message}")
            }
        }
    }

    private fun stopAdvertisement(advertiseId: Long, invoke: Invoke) {
        val session = advertiseMap[advertiseId]
        if (session != null) {
            synchronized(session) {
                if (advertiseMap[advertiseId] === session) {
                    advertiseMap.remove(advertiseId)
                    val wasStarting = session.state == NativeSessionState.STARTING
                    session.state = NativeSessionState.CLOSED
                    if (wasStarting) {
                        session.startInvoke.reject("DNS-SD advertisement stopped before becoming ready")
                    }
                    val update = session.pendingUpdate
                    session.pendingUpdate = null
                    update?.invoke?.reject("Peer advertisement is closed")
                    // An UNREGISTERING update has already issued this call.
                    if (update?.phase != AdvertisementUpdatePhase.UNREGISTERING) {
                        try {
                            unregisterRegistrationOnce(
                                session,
                                session.generation,
                                session.listener
                            )
                        } catch (_: Throwable) {}
                    }
                }
            }
        }
        invoke.resolve()
    }

    @Command
    fun advertise_peer_stop(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseStopArgs::class.java)
        stopAdvertisement(args.advertiseId, invoke)
    }

    // ── Generic DNS-SD ────────────────────────────────────────────────────────

    private fun protoSuffix(protocol: String): String =
        if (protocol.equals("tcp", ignoreCase = true)) "_tcp" else "_udp"

    /**
     * Format a resolved host + port as a socket-address string. Numeric IPv6
     * scopes are preserved; a link-local address without a numeric scope is
     * subsequently rejected by `validatedSocketLiteral`.
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

    @Command
    fun browse_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(DnsSdBrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"
        val startInvoke = InvokeOnce(invoke)

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-dnssd", "browse $browseId start failed: $errorCode")
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    val message = "Failed to start DNS-SD browse: error $errorCode"
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    session.state = NativeSessionState.FAILED
                    session.terminalError = message
                    retireResolveRequests(session)
                    if (failedBeforeReady) {
                        dnsSdBrowseMap.remove(browseId)
                        session.startInvoke.reject(message)
                        // Start failure is already terminal. In particular,
                        // API 21 removes the listener after this callback; a
                        // nested stop would target a failed discovery key.
                    }
                }
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (dnsSdBrowseMap[browseId] !== session) return
                    val failedBeforeReady = session.state == NativeSessionState.STARTING
                    session.state = NativeSessionState.FAILED
                    session.terminalError = "DNS-SD browse stop failed: error $errorCode"
                    retireResolveRequests(session)
                    if (failedBeforeReady) {
                        dnsSdBrowseMap.remove(browseId)
                        session.startInvoke.reject(session.terminalError!!)
                    }
                }
            }
            override fun onDiscoveryStarted(serviceType: String) {
                val session = dnsSdBrowseMap[browseId] ?: return
                synchronized(session) {
                    if (
                        dnsSdBrowseMap[browseId] !== session ||
                        session.state != NativeSessionState.STARTING
                    ) return
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
                    if (session.state == NativeSessionState.STARTING) {
                        session.state = NativeSessionState.CLOSED
                        dnsSdBrowseMap.remove(browseId)
                        session.startInvoke.reject("DNS-SD browse stopped before becoming ready")
                    } else if (session.state == NativeSessionState.ACTIVE) {
                        session.state = NativeSessionState.CLOSED
                    }
                    retireResolveRequests(session)
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
                    ResolveOwner.Generic(session, instanceName, generation),
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
                synchronized(session) {
                    if (
                        dnsSdBrowseMap[browseId] !== session ||
                        session.state != NativeSessionState.ACTIVE
                    ) return
                    session.presenceGenerations.remove(name)
                    if (!session.knownInstances.remove(name)) return
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
        }

        val session = DnsSdBrowseSession(browseId, manager, listener, serviceType, startInvoke)
        dnsSdBrowseMap[browseId] = session

        try {
            manager.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Throwable) {
            synchronized(session) {
                if (dnsSdBrowseMap[browseId] === session) dnsSdBrowseMap.remove(browseId)
                session.state = NativeSessionState.FAILED
                session.terminalError = e.message
                retireResolveRequests(session)
                session.startInvoke.reject("Discovery failed: ${e.message}")
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
                    session.state == NativeSessionState.CLOSED ||
                    session.state == NativeSessionState.FAILED
                ) {
                    if (dnsSdBrowseMap[args.browseId] === session) {
                        dnsSdBrowseMap.remove(args.browseId)
                    }
                }
            }
        }
        invoke.resolve(ret)
    }

    @Command
    fun browse_stop(invoke: Invoke) {
        val args = invoke.parseArgs(BrowseStopArgs::class.java)
        val session = dnsSdBrowseMap.remove(args.browseId)
        if (session != null) {
            synchronized(session) {
                val wasStarting = session.state == NativeSessionState.STARTING
                session.state = NativeSessionState.CLOSED
                session.presenceGenerations.clear()
                retireResolveRequests(session)
                if (wasStarting) {
                    session.startInvoke.reject("DNS-SD browse stopped before becoming ready")
                }
            }
            try { session.manager.stopServiceDiscovery(session.listener) } catch (_: Throwable) {}
        }
        invoke.resolve()
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
        startAdvertisement(manager, advertiseId, AdvertisementKind.Generic, info, invoke)
    }

    @Command
    fun advertise_stop(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseStopArgs::class.java)
        stopAdvertisement(args.advertiseId, invoke)
    }
}
