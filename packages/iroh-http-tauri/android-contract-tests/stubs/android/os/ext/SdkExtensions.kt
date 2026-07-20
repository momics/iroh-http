package android.os.ext

object SdkExtensions {
    private val versions = mutableMapOf<Int, Int>()

    fun getExtensionVersion(extension: Int): Int = versions[extension] ?: 0

    fun setExtensionVersion(extension: Int, version: Int) {
        versions[extension] = version
    }
}
