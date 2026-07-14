package android.content

open class Context {
    open fun getSystemService(name: String): Any? = null

    companion object {
        const val NSD_SERVICE: String = "nsd"
        const val CONNECTIVITY_SERVICE: String = "connectivity"
    }
}
