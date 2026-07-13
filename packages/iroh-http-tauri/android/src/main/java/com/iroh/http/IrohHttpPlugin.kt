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
import java.util.concurrent.ConcurrentHashMap
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

    // #346: primary direct `ip:port` address to publish so browsing peers can
    // dial this node over the LAN. Carries the real bound QUIC port (never 0).
    var address: String? = null
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

    private val nextBrowseId = AtomicLong(1)
    private val nextAdvertiseId = AtomicLong(1)

    private data class BrowseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.DiscoveryListener,
        val pendingEvents: MutableList<JSObject> = mutableListOf(),
        // fullServiceName → snapshot. Keyed by a signature of (nodeId + sorted
        // dialable addrs) so a peer that rebinds to a new address under the SAME
        // nodeId (the iOS foreground-restart-rebind case, #336) re-emits instead
        // of being suppressed by a nodeId-only dedup (#350 review M2).
        val knownNodes: ConcurrentHashMap<String, PeerSnapshot> = ConcurrentHashMap()
    )

    private data class PeerSnapshot(val nodeId: String, val signature: String)

    private data class AdvertiseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.RegistrationListener
    )

    /** A generic DNS-SD browse session, carrying full records rather than peers. */
    private data class DnsSdBrowseSession(
        val id: Long,
        val manager: NsdManager,
        val listener: NsdManager.DiscoveryListener,
        val serviceType: String,
        val pendingRecords: MutableList<JSObject> = mutableListOf(),
        // name → snapshot signature (txt + addrs). A Map, not a Set, so a later
        // TXT/addr change (peer rebinds to a new port, re-advertises a new
        // `address`) re-emits instead of being suppressed forever — parity with
        // the iOS DnsSdBrowseSession snapshot (#350 review W2).
        val knownInstances: MutableMap<String, String> =
            java.util.Collections.synchronizedMap(mutableMapOf())
    )

    private val browseMap = ConcurrentHashMap<Long, BrowseSession>()
    private val advertiseMap = ConcurrentHashMap<Long, AdvertiseSession>()
    private val dnsSdBrowseMap = ConcurrentHashMap<Long, DnsSdBrowseSession>()

    private fun nsd(): NsdManager? =
        activity.getSystemService(Context.NSD_SERVICE) as? NsdManager

    // ── DNS ───────────────────────────────────────────────────────────────────

    /**
     * Return the active network's configured DNS servers (IP strings).
     *
     * iroh's Rust DNS resolver cannot read Android's system DNS config (there is
     * no `/etc/resolv.conf`; servers live in `LinkProperties`, reachable only
     * via JNI/`ndk_context`). The Rust side calls this at endpoint creation and
     * configures iroh's resolver with the returned servers so relay, pkarr, and
     * DNS-discovery lookups resolve instead of timing out.
     */
    @Command
    fun get_dns_servers(invoke: Invoke) {
        val servers = JSONArray()
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
                    for (addr in props.dnsServers) {
                        // Strip any IPv6 zone/scope suffix (e.g. `fe80::1%wlan0`)
                        // so the Rust `IpAddr` parser in bind.rs accepts it —
                        // matches formatSocketAddr's handling for socket addrs.
                        val host = addr.hostAddress?.substringBefore('%')
                        if (!host.isNullOrEmpty()) servers.put(host)
                    }
                }
            }
        } catch (e: Throwable) {
            // Catch Throwable (not just Exception) so a linkage/verification
            // Error on an unexpected OS version degrades to the default
            // resolver instead of crashing the app (#350 F2).
            Log.e("iroh-http-dns", "get_dns_servers failed: ${e.message}")
        }
        val ret = JSObject()
        ret.put("servers", servers)
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
    private val resolveQueue = java.util.ArrayDeque<Pair<NsdServiceInfo, NsdManager.ResolveListener>>()
    private var resolveInProgress = false

    private fun enqueueResolve(serviceInfo: NsdServiceInfo, listener: NsdManager.ResolveListener) {
        synchronized(resolveQueue) {
            resolveQueue.addLast(Pair(serviceInfo, listener))
            if (resolveInProgress) return
            resolveInProgress = true
        }
        drainResolveQueue()
    }

    private fun drainResolveQueue() {
        val next: Pair<NsdServiceInfo, NsdManager.ResolveListener> =
            synchronized(resolveQueue) {
                val polled = resolveQueue.pollFirst()
                if (polled == null) {
                    resolveInProgress = false
                    return
                }
                polled
            }
        val manager = nsd()
        if (manager == null) {
            drainResolveQueue()
            return
        }
        val (serviceInfo, listener) = next
        // #334: greppable trace of the serialized resolve queue. `depth` is the
        // number of resolves still queued behind this one — proof that
        // concurrent resolves are serialized rather than dropped with
        // FAILURE_ALREADY_ACTIVE.
        val depth = synchronized(resolveQueue) { resolveQueue.size }
        Log.d(
            "IROH_DNSSD_CHECK",
            "resolve dequeue instance=${serviceInfo.serviceName} depth=$depth",
        )
        val wrapped = object : NsdManager.ResolveListener {
            override fun onServiceResolved(resolved: NsdServiceInfo) {
                Log.d(
                    "IROH_DNSSD_CHECK",
                    "resolve ok instance=${resolved.serviceName} port=${resolved.port}",
                )
                try { listener.onServiceResolved(resolved) } finally { drainResolveQueue() }
            }
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.d(
                    "IROH_DNSSD_CHECK",
                    "resolve fail instance=${serviceInfo.serviceName} errorCode=$errorCode",
                )
                try { listener.onResolveFailed(serviceInfo, errorCode) } finally { drainResolveQueue() }
            }
        }
        try {
            manager.resolveService(serviceInfo, wrapped)
        } catch (e: Exception) {
            drainResolveQueue()
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

    // ── Browse ────────────────────────────────────────────────────────────────

    @Command
    fun browse_peers_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(BrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}._udp"

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-mdns", "browse $browseId start failed: $errorCode")
                // #350 review L4: drop the session so a failed browse doesn't
                // leak in the map.
                browseMap.remove(browseId)
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onDiscoveryStopped(serviceType: String) {}

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                val session = browseMap[browseId] ?: return
                enqueueResolve(serviceInfo, object : NsdManager.ResolveListener {
                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        // Prefer the `pk` attribute (set by every current
                        // advertiser, mobile and desktop alike); fall back to
                        // the DNS-SD instance name — which desktop's
                        // `mdns-sd`-backed advertiser publishes as the base32
                        // endpoint id too — for advertisements from older or
                        // third-party peers that carry no `pk` attribute.
                        val pkAttr = resolved.attributes["pk"]?.let { String(it) }
                        val nodeId = if (!pkAttr.isNullOrEmpty()) {
                            pkAttr
                        } else {
                            val name = resolved.serviceName
                            if (isValidEndpointId(name)) name else return
                        }

                        val key = resolved.serviceName
                        val addrs = JSONArray()
                        // #346: a direct `ip:port` address published by the
                        // advertiser lets this peer be dialed over the LAN. It
                        // already carries the real bound QUIC port.
                        var hasAddressTxt = false
                        resolved.attributes["address"]?.let { b ->
                            val address = String(b)
                            if (address.isNotEmpty()) {
                                addrs.put(address)
                                hasAddressTxt = true
                            }
                        }
                        // #350 review W1/F12: the resolved SRV host:port is only
                        // a FALLBACK for advertisers that publish no `address`
                        // TXT (e.g. a desktop peer whose real QUIC port rides in
                        // the SRV record). When an `address` TXT is present it
                        // already carries the real bound QUIC port, so appending
                        // SRV would inject a bogus second target — Android's own
                        // advertiser uses SRV port 1 and iOS's NWListener port is
                        // unrelated to QUIC. Also skip placeholder SRV ports
                        // (<=1), which are never a real QUIC socket.
                        val hostAddr = resolved.host?.hostAddress
                        if (!hasAddressTxt && !hostAddr.isNullOrEmpty() && resolved.port > 1) {
                            addrs.put(formatSocketAddr(hostAddr, resolved.port))
                        }
                        resolved.attributes["relay"]?.let { b ->
                            val relay = String(b)
                            if (relay.isNotEmpty()) addrs.put(relay)
                        }

                        // #350 review M2: dedup on (nodeId + sorted addrs) so a
                        // rebind under the same nodeId re-emits with the new
                        // address instead of being suppressed.
                        val sortedAddrs = (0 until addrs.length())
                            .map { addrs.getString(it) }
                            .sorted()
                        val signature = "$nodeId|${sortedAddrs.joinToString(",")}"
                        if (session.knownNodes[key]?.signature == signature) return
                        session.knownNodes[key] = PeerSnapshot(nodeId, signature)

                        val event = JSObject()
                        event.put("type", "discovered")
                        event.put("nodeId", nodeId)
                        event.put("addrs", addrs)
                        synchronized(session.pendingEvents) { session.pendingEvents.add(event) }
                    }

                    override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
                })
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                val session = browseMap[browseId] ?: return
                val snapshot = session.knownNodes.remove(serviceInfo.serviceName) ?: return
                val event = JSObject()
                event.put("type", "expired")
                event.put("nodeId", snapshot.nodeId)
                event.put("addrs", JSONArray())
                synchronized(session.pendingEvents) { session.pendingEvents.add(event) }
            }
        }

        val session = BrowseSession(browseId, manager, listener)
        browseMap[browseId] = session

        try {
            manager.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            browseMap.remove(browseId)
            return invoke.reject("Discovery failed: ${e.message}")
        }

        val ret = JSObject()
        ret.put("browseId", browseId)
        invoke.resolve(ret)
    }

    @Command
    fun browse_peers_poll(invoke: Invoke) {
        val args = invoke.parseArgs(BrowsePollArgs::class.java)
        val session = browseMap[args.browseId]
        val ret = JSObject()
        if (session == null) {
            ret.put("events", JSONArray())
        } else {
            val events: List<JSObject>
            synchronized(session.pendingEvents) {
                events = session.pendingEvents.toList()
                session.pendingEvents.clear()
            }
            val arr = JSONArray()
            events.forEach { arr.put(it) }
            ret.put("events", arr)
        }
        invoke.resolve(ret)
    }

    @Command
    fun browse_peers_stop(invoke: Invoke) {
        val args = invoke.parseArgs(BrowseStopArgs::class.java)
        val session = browseMap.remove(args.browseId)
        if (session != null) {
            try { session.manager.stopServiceDiscovery(session.listener) } catch (_: Exception) {}
        }
        invoke.resolve()
    }

    // ── Advertise ─────────────────────────────────────────────────────────────

    @Command
    fun advertise_peer_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(AdvertiseStartArgs::class.java)
        val advertiseId = nextAdvertiseId.getAndIncrement()
        val serviceType = "_${args.serviceName}._udp"

        val info = NsdServiceInfo().apply {
            serviceName = args.pk.take(63)
            this.serviceType = serviceType
            setPort(1)  // placeholder; iroh-http connections use node-ID, not port
            setAttribute("pk", args.pk)
            args.relay?.let { setAttribute("relay", it) }
            // #346: publish the node's direct `ip:port` so peers can dial it
            // over the LAN. The SRV port above is a placeholder (connections use
            // node-id, not the SRV port), so the reconciled address — carrying
            // the real bound QUIC port — travels in this `address` TXT instead.
            args.address?.let { setAttribute("address", it) }
        }

        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {}
            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.e("iroh-http-mdns", "advertise $advertiseId failed: $errorCode")
                // #350 review L4: drop the session so a failed advertise doesn't
                // leak in the map.
                advertiseMap.remove(advertiseId)
            }
            override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {}
            override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
        }

        advertiseMap[advertiseId] = AdvertiseSession(advertiseId, manager, listener)
        try {
            manager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            advertiseMap.remove(advertiseId)
            return invoke.reject("Registration failed: ${e.message}")
        }

        val ret = JSObject()
        ret.put("advertiseId", advertiseId)
        invoke.resolve(ret)
    }

    @Command
    fun advertise_peer_stop(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseStopArgs::class.java)
        val session = advertiseMap.remove(args.advertiseId)
        if (session != null) {
            try { session.manager.unregisterService(session.listener) } catch (_: Exception) {}
        }
        invoke.resolve()
    }

    // ── Generic DNS-SD ────────────────────────────────────────────────────────

    private fun protoSuffix(protocol: String): String =
        if (protocol.equals("tcp", ignoreCase = true)) "_tcp" else "_udp"

    /**
     * Format a host + port as a dialable socket-address string (#346). IPv6
     * literals are bracketed and any interface scope suffix (`%wlan0`) is
     * stripped so the result parses as a Rust `SocketAddr`.
     */
    private fun formatSocketAddr(host: String, port: Int): String =
        if (host.contains(':')) {
            "[${host.substringBefore('%')}]:$port"
        } else {
            "$host:$port"
        }

    @Command
    fun browse_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(DnsSdBrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-dnssd", "browse $browseId start failed: $errorCode")
                // #350 review L4: drop the session so a failed browse doesn't
                // leak in the map.
                dnsSdBrowseMap.remove(browseId)
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onDiscoveryStopped(serviceType: String) {}

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                val session = dnsSdBrowseMap[browseId] ?: return
                enqueueResolve(serviceInfo, object : NsdManager.ResolveListener {
                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        val name = resolved.serviceName

                        val txt = JSObject()
                        resolved.attributes?.forEach { (k, v) ->
                            txt.put(k, if (v != null) String(v) else "")
                        }

                        val addrs = JSONArray()
                        val hostAddr = resolved.host?.hostAddress
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
                        if (!hostAddr.isNullOrEmpty() && resolved.port > 1) {
                            addrs.put(formatSocketAddr(hostAddr, resolved.port))
                        }

                        // #350 review W2: re-emit only when the record actually
                        // changed. A stable signature over the sorted TXT and the
                        // resolved addrs lets a rebind/re-advertise surface again.
                        // #350 F29: length-prefix every field (netstring style)
                        // so the snapshot is injective — a delimiter-joined form
                        // lets TXT {a:"b;c=d"} and {a:"b",c:"d"} collide and
                        // suppresses a real update.
                        fun StringBuilder.field(s: String) {
                            append(s.length).append(':').append(s)
                        }
                        val signature = buildString {
                            resolved.attributes?.toSortedMap()?.forEach { (k, v) ->
                                field(k)
                                field(if (v != null) String(v) else "")
                            }
                            append('|')
                            for (i in 0 until addrs.length()) field(addrs.getString(i))
                        }
                        if (session.knownInstances.put(name, signature) == signature) return

                        val record = JSObject()
                        record.put("isActive", true)
                        record.put("serviceType", session.serviceType)
                        record.put("instanceName", name)
                        record.put("host", hostAddr ?: JSONObject.NULL)
                        record.put("port", resolved.port)
                        record.put("addrs", addrs)
                        record.put("txt", txt)
                        synchronized(session.pendingRecords) { session.pendingRecords.add(record) }
                    }

                    override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
                })
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                val session = dnsSdBrowseMap[browseId] ?: return
                val name = serviceInfo.serviceName
                if (session.knownInstances.remove(name) == null) return
                val record = JSObject()
                record.put("isActive", false)
                record.put("serviceType", session.serviceType)
                record.put("instanceName", name)
                record.put("host", JSONObject.NULL)
                record.put("port", 0)
                record.put("addrs", JSONArray())
                record.put("txt", JSObject())
                synchronized(session.pendingRecords) { session.pendingRecords.add(record) }
            }
        }

        val session = DnsSdBrowseSession(browseId, manager, listener, serviceType)
        dnsSdBrowseMap[browseId] = session

        try {
            manager.discoverServices(serviceType, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            dnsSdBrowseMap.remove(browseId)
            return invoke.reject("Discovery failed: ${e.message}")
        }

        val ret = JSObject()
        ret.put("browseId", browseId)
        invoke.resolve(ret)
    }

    @Command
    fun browse_poll(invoke: Invoke) {
        val args = invoke.parseArgs(BrowsePollArgs::class.java)
        val session = dnsSdBrowseMap[args.browseId]
        val ret = JSObject()
        if (session == null) {
            ret.put("records", JSONArray())
        } else {
            val records: List<JSObject>
            synchronized(session.pendingRecords) {
                records = session.pendingRecords.toList()
                session.pendingRecords.clear()
            }
            val arr = JSONArray()
            records.forEach { arr.put(it) }
            ret.put("records", arr)
        }
        invoke.resolve(ret)
    }

    @Command
    fun browse_stop(invoke: Invoke) {
        val args = invoke.parseArgs(BrowseStopArgs::class.java)
        val session = dnsSdBrowseMap.remove(args.browseId)
        if (session != null) {
            try { session.manager.stopServiceDiscovery(session.listener) } catch (_: Exception) {}
        }
        invoke.resolve()
    }

    @Command
    fun advertise_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(DnsSdAdvertiseStartArgs::class.java)
        val advertiseId = nextAdvertiseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"

        val info = NsdServiceInfo().apply {
            serviceName = args.instanceName.take(63)
            this.serviceType = serviceType
            setPort(if (args.port > 0) args.port else 1)
            args.txt.forEach { (k, v) -> setAttribute(k, v) }
        }

        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {}
            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.e("iroh-http-dnssd", "advertise $advertiseId failed: $errorCode")
                // #350 review L4: drop the session so a failed advertise doesn't
                // leak in the map.
                advertiseMap.remove(advertiseId)
            }
            override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {}
            override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
        }

        advertiseMap[advertiseId] = AdvertiseSession(advertiseId, manager, listener)
        try {
            manager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            advertiseMap.remove(advertiseId)
            return invoke.reject("Registration failed: ${e.message}")
        }

        val ret = JSObject()
        ret.put("advertiseId", advertiseId)
        invoke.resolve(ret)
    }

    @Command
    fun advertise_stop(invoke: Invoke) {
        val args = invoke.parseArgs(AdvertiseStopArgs::class.java)
        val session = advertiseMap.remove(args.advertiseId)
        if (session != null) {
            try { session.manager.unregisterService(session.listener) } catch (_: Exception) {}
        }
        invoke.resolve()
    }
}
