package android.os

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
        scheduler.schedule(runnable, delayMillis, TimeUnit.MILLISECONDS)
        return true
    }

    companion object {
        private val scheduler = Executors.newSingleThreadScheduledExecutor { runnable ->
            Thread(runnable, "android-main-looper-stub").apply { isDaemon = true }
        }
    }
}
