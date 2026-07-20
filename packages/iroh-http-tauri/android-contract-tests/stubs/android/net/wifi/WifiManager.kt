package android.net.wifi

open class WifiManager {
    open fun createMulticastLock(tag: String): MulticastLock = MulticastLock()

    open class MulticastLock {
        open var isHeld: Boolean = false
            protected set

        open fun setReferenceCounted(refCounted: Boolean) {}

        open fun acquire() {
            isHeld = true
        }

        open fun release() {
            isHeld = false
        }
    }
}
