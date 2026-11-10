package io.liliadb.reactnative

import com.facebook.react.bridge.*
import io.liliadb.FfiResourceProfile
import io.liliadb.LiliaDatabase
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import java.io.File
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

class LiliaReactNativeModule(reactContext: ReactApplicationContext) :
    ReactContextBaseJavaModule(reactContext) {

    private val instances = ConcurrentHashMap<String, LiliaDatabase>()
    private val scope = CoroutineScope(Dispatchers.IO)

    override fun getName(): String = "LiliaReactNativeModule"

    private fun resolveProfile(profileStr: String?): FfiResourceProfile {
        return when (profileStr?.lowercase()) {
            "constrained" -> FfiResourceProfile.CONSTRAINED
            "server" -> FfiResourceProfile.SERVER
            else -> FfiResourceProfile.MOBILE
        }
    }

    @ReactMethod
    fun openDatabase(options: ReadableMap, promise: Promise) {
        val path = options.getString("path") ?: run {
            promise.reject("INVALID_INPUT", "Path is required")
            return
        }

        val profile = resolveProfile(options.getString("profile"))
        val useSecureStorage = options.hasKey("useSecureStorage") && options.getBoolean("useSecureStorage")

        scope.launch {
            try {
                val encKey: ByteArray? = if (useSecureStorage) {
                    val dbName = File(path).name
                    KeystoreHelper.getOrCreateKey(dbName)
                } else if (options.hasKey("encryptionKeyHex")) {
                    val hex = options.getString("encryptionKeyHex") ?: ""
                    if (hex.length == 64) hexToBytes(hex) else null
                } else {
                    null
                }

                val db = if (options.hasKey("passphrase")) {
                    val passphrase = options.getString("passphrase") ?: ""
                    LiliaDatabase.openWithPassphrase(path, profile, passphrase)
                } else {
                    LiliaDatabase.open(path, profile, encKey)
                }

                val handle = UUID.randomUUID().toString()
                instances[handle] = db

                val result = Arguments.createMap().apply {
                    putString("handle", handle)
                    putString("path", path)
                }
                promise.resolve(result)
            } catch (e: Exception) {
                promise.reject("OPEN_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun prepareForBackground(handle: String, promise: Promise) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        try {
            db.prepareForBackground()
            promise.resolve(true)
        } catch (e: Exception) {
            promise.reject("FLUSH_ERROR", e.message, e)
        }
    }

    @ReactMethod
    fun kvSet(
        handle: String,
        namespace: String,
        keyHex: String,
        valHex: String,
        expiresAtMs: Double?,
        promise: Promise
    ) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        scope.launch {
            try {
                val key = hexToBytes(keyHex)
                val value = hexToBytes(valHex)
                val expires = expiresAtMs?.toLong()
                val version = db.kvSet(namespace, key, value, expires)
                promise.resolve(version.toDouble())
            } catch (e: Exception) {
                promise.reject("KV_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun kvGet(handle: String, namespace: String, keyHex: String, promise: Promise) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        scope.launch {
            try {
                val key = hexToBytes(keyHex)
                val entry = db.kvGet(namespace, key)
                if (entry != null) {
                    val map = Arguments.createMap().apply {
                        putString("namespace", entry.namespace)
                        putString("keyHex", bytesToHex(entry.key))
                        putString("valueHex", bytesToHex(entry.value))
                        putDouble("version", entry.version.toDouble())
                        if (entry.expiresAtMs != null) {
                            putDouble("expiresAtMs", entry.expiresAtMs!!.toDouble())
                        }
                    }
                    promise.resolve(map)
                } else {
                    promise.resolve(null)
                }
            } catch (e: Exception) {
                promise.reject("KV_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun jsonPut(handle: String, space: String, id: String, valueJson: String, promise: Promise) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        scope.launch {
            try {
                val version = db.jsonPut(space, id, valueJson)
                promise.resolve(version.toDouble())
            } catch (e: Exception) {
                promise.reject("JSON_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun jsonGet(handle: String, space: String, id: String, promise: Promise) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        scope.launch {
            try {
                val doc = db.jsonGet(space, id)
                if (doc != null) {
                    val map = Arguments.createMap().apply {
                        putString("space", doc.space)
                        putString("id", doc.id)
                        putString("valueJson", doc.valueJson)
                        putDouble("version", doc.version.toDouble())
                    }
                    promise.resolve(map)
                } else {
                    promise.resolve(null)
                }
            } catch (e: Exception) {
                promise.reject("JSON_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun sqlQuery(handle: String, sql: String, paramsJson: String?, promise: Promise) {
        val db = instances[handle] ?: run {
            promise.reject("NOT_FOUND", "Database handle not found")
            return
        }
        scope.launch {
            try {
                val res = db.sqlQuery(sql, paramsJson)
                val columns = Arguments.createArray().apply {
                    res.columns.forEach { pushString(it) }
                }
                val rows = Arguments.createArray().apply {
                    res.rowsJson.forEach { pushString(it) }
                }
                val map = Arguments.createMap().apply {
                    putArray("columns", columns)
                    putArray("rowsJson", rows)
                    putDouble("rowsAffected", res.rowsAffected.toDouble())
                }
                promise.resolve(map)
            } catch (e: Exception) {
                promise.reject("SQL_ERROR", e.message, e)
            }
        }
    }

    @ReactMethod
    fun close(handle: String, timeoutMs: Double, promise: Promise) {
        val db = instances.remove(handle) ?: run {
            promise.resolve(true)
            return
        }
        scope.launch {
            try {
                db.close(timeoutMs.toULong())
                promise.resolve(true)
            } catch (e: Exception) {
                promise.reject("CLOSE_ERROR", e.message, e)
            }
        }
    }

    private fun hexToBytes(hex: String): ByteArray {
        val result = ByteArray(hex.length / 2)
        for (i in result.indices) {
            val index = i * 2
            result[i] = hex.substring(index, index + 2).toInt(16).toByte()
        }
        return result
    }

    private fun bytesToHex(bytes: ByteArray): String {
        return bytes.joinToString("") { "%02x".format(it) }
    }
}
