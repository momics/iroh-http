package org.json

class JSONArray() {
    private val values: MutableList<Any?> = mutableListOf()

    constructor(items: Collection<*>) : this() {
        values.addAll(items)
    }

    fun put(value: Any?): JSONArray {
        values.add(value)
        return this
    }

    fun length(): Int = values.size
    fun getString(index: Int): String = values[index] as String
    operator fun get(index: Int): Any? = values[index]
    fun toList(): List<Any?> = values.toList()
}

class JSONObject {
    companion object {
        val NULL: Any = Any()
    }
}
