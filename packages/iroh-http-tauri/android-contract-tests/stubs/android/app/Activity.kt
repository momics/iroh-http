package android.app

import android.content.Context

open class Activity : Context() {
    private val services = mutableMapOf<String, Any>()

    fun setSystemService(name: String, service: Any) {
        services[name] = service
    }

    override fun getSystemService(name: String): Any? = services[name]
}
