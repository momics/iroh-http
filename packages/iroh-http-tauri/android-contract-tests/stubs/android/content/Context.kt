package android.content

import android.content.res.Configuration
import android.content.res.Resources

open class Context {
    open val resources: Resources = Resources()

    open fun getSystemService(name: String): Any? = null

    open fun createConfigurationContext(configuration: Configuration): Context = this

    companion object {
        const val NSD_SERVICE: String = "nsd"
        const val CONNECTIVITY_SERVICE: String = "connectivity"
        const val WIFI_SERVICE: String = "wifi"
    }
}
