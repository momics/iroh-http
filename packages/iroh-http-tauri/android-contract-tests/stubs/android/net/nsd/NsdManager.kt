package android.net.nsd

import java.net.InetAddress
import java.nio.charset.StandardCharsets

open class NsdServiceInfo {
    var serviceName: String = ""
    var serviceType: String = ""
    private var storedPort: Int = 0
    val port: Int
        get() = storedPort
    var host: InetAddress? = null
    val attributes: MutableMap<String, ByteArray?> = linkedMapOf()

    fun setPort(value: Int) {
        storedPort = value
    }

    fun setAttribute(key: String, value: String) {
        val keyBytes = key.toByteArray(StandardCharsets.UTF_8).size
        val valueBytes = value.toByteArray(StandardCharsets.UTF_8).size
        require(key.isNotEmpty() && !key.contains('=')) { "invalid TXT key" }
        require(keyBytes + 1 + valueBytes <= 255) { "TXT entry exceeds 255 bytes" }
        attributes[key] = value.toByteArray(StandardCharsets.UTF_8)
    }
}

open class NsdManager {
    interface DiscoveryListener {
        fun onStartDiscoveryFailed(serviceType: String, errorCode: Int)
        fun onStopDiscoveryFailed(serviceType: String, errorCode: Int)
        fun onDiscoveryStarted(serviceType: String)
        fun onDiscoveryStopped(serviceType: String)
        fun onServiceFound(serviceInfo: NsdServiceInfo)
        fun onServiceLost(serviceInfo: NsdServiceInfo)
    }

    interface ResolveListener {
        fun onServiceResolved(resolved: NsdServiceInfo)
        fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int)
    }

    interface RegistrationListener {
        fun onServiceRegistered(serviceInfo: NsdServiceInfo)
        fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int)
        fun onServiceUnregistered(serviceInfo: NsdServiceInfo)
        fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int)
    }

    open fun discoverServices(
        serviceType: String,
        protocolType: Int,
        listener: DiscoveryListener
    ) {}

    open fun stopServiceDiscovery(listener: DiscoveryListener) {}

    open fun resolveService(serviceInfo: NsdServiceInfo, listener: ResolveListener) {}

    open fun registerService(
        serviceInfo: NsdServiceInfo,
        protocolType: Int,
        listener: RegistrationListener
    ) {}

    open fun unregisterService(listener: RegistrationListener) {}

    companion object {
        const val PROTOCOL_DNS_SD: Int = 1
    }
}
