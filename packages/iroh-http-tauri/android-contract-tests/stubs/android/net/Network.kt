package android.net

import java.net.InetAddress

open class Network

class LinkAddress(val address: InetAddress)

open class LinkProperties {
    var interfaceName: String? = null
    val dnsServers: MutableList<InetAddress> = mutableListOf()
    val linkAddresses: MutableList<LinkAddress> = mutableListOf()
}

open class ConnectivityManager {
    open val activeNetwork: Network? = null
    open val allNetworks: Array<Network> = emptyArray()
    open fun getLinkProperties(network: Network): LinkProperties? = null
}
