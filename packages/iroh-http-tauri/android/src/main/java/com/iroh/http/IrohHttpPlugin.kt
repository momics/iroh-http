package com.iroh.http

import android.app.Activity
import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
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
        val knownNodes: ConcurrentHashMap<String, String> = ConcurrentHashMap()
    )

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
        val knownInstances: MutableSet<String> = java.util.Collections.synchronizedSet(mutableSetOf())
    )

    private val browseMap = ConcurrentHashMap<Long, BrowseSession>()
    private val advertiseMap = ConcurrentHashMap<Long, AdvertiseSession>()
    private val dnsSdBrowseMap = ConcurrentHashMap<Long, DnsSdBrowseSession>()

    private fun nsd(): NsdManager? =
        activity.getSystemService(Context.NSD_SERVICE) as? NsdManager

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
        val wrapped = object : NsdManager.ResolveListener {
            override fun onServiceResolved(resolved: NsdServiceInfo) {
                try { listener.onServiceResolved(resolved) } finally { drainResolveQueue() }
            }
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
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
                        if (session.knownNodes[key] == nodeId) return

                        session.knownNodes[key] = nodeId
                        val addrs = JSONArray()
                        resolved.attributes["relay"]?.let { b ->
                            val relay = String(b)
                            if (relay.isNotEmpty()) addrs.put(relay)
                        }

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
                val pk = session.knownNodes.remove(serviceInfo.serviceName) ?: return
                val event = JSObject()
                event.put("type", "expired")
                event.put("nodeId", pk)
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
        }

        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {}
            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                Log.e("iroh-http-mdns", "advertise $advertiseId failed: $errorCode")
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

    @Command
    fun browse_start(invoke: Invoke) {
        val manager = nsd() ?: return invoke.reject("NsdManager unavailable")
        val args = invoke.parseArgs(DnsSdBrowseStartArgs::class.java)
        val browseId = nextBrowseId.getAndIncrement()
        val serviceType = "_${args.serviceName}.${protoSuffix(args.protocol)}"

        val listener = object : NsdManager.DiscoveryListener {
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("iroh-http-dnssd", "browse $browseId start failed: $errorCode")
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onDiscoveryStopped(serviceType: String) {}

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                val session = dnsSdBrowseMap[browseId] ?: return
                enqueueResolve(serviceInfo, object : NsdManager.ResolveListener {
                    override fun onServiceResolved(resolved: NsdServiceInfo) {
                        val name = resolved.serviceName
                        if (!session.knownInstances.add(name)) return

                        val txt = JSObject()
                        resolved.attributes?.forEach { (k, v) ->
                            txt.put(k, if (v != null) String(v) else "")
                        }

                        val addrs = JSONArray()
                        val hostAddr = resolved.host?.hostAddress
                        if (!hostAddr.isNullOrEmpty()) addrs.put(hostAddr)

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
                if (!session.knownInstances.remove(name)) return
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
