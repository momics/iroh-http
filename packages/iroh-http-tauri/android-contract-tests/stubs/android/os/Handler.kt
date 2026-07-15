package android.os

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class Looper private constructor() {
    companion object {
        private val main = Looper()

        @JvmStatic
        fun getMainLooper(): Looper = main
    }
}

class Handler(@Suppress("UNUSED_PARAMETER") looper: Looper) {
    fun postDelayed(runnable: Runnable, delayMillis: Long): Boolean {
        postDelayedEntered?.countDown()
        postDelayedRelease?.let { release ->
            check(release.await(5, TimeUnit.SECONDS)) { "postDelayed barrier timed out" }
        }
        val future = scheduler.schedule({
            scheduled.remove(runnable)
            runnable.run()
        }, delayMillis, TimeUnit.MILLISECONDS)
        scheduled[runnable] = future
        return true
    }

    fun removeCallbacks(runnable: Runnable) {
        scheduled.remove(runnable)?.cancel(false)
    }

    companion object {
        private val scheduled =
            ConcurrentHashMap<Runnable, java.util.concurrent.ScheduledFuture<*>>()
        private val scheduler = Executors.newSingleThreadScheduledExecutor { runnable ->
            Thread(runnable, "android-main-looper-stub").apply { isDaemon = true }
        }
        @Volatile var postDelayedEntered: CountDownLatch? = null
        @Volatile var postDelayedRelease: CountDownLatch? = null

        /** Deterministically fire platform-delayed callbacks in contract tests. */
        fun runAllPending() {
            val pending = scheduled.keys.toList()
            pending.forEach { runnable ->
                scheduled.remove(runnable)?.cancel(false)
                runnable.run()
            }
        }
    }
}
