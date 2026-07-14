package app.tauri.plugin

import android.app.Activity

open class Plugin(activity: Activity)

open class Invoke(private val args: Any? = null) {
    val resolutions: MutableList<JSObject?> = mutableListOf()
    val rejections: MutableList<String> = mutableListOf()

    @Suppress("UNCHECKED_CAST")
    fun <T> parseArgs(type: Class<T>): T = args as T

    open fun resolve(payload: JSObject) {
        resolutions.add(payload)
    }

    open fun resolve() {
        resolutions.add(null)
    }

    open fun reject(message: String) {
        rejections.add(message)
    }

    val completionCount: Int
        get() = resolutions.size + rejections.size
}

class JSObject {
    private val values: MutableMap<String, Any?> = linkedMapOf()

    fun put(key: String, value: Any?): JSObject {
        values[key] = value
        return this
    }

    operator fun get(key: String): Any? = values[key]
}
