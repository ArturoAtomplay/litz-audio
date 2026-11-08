package audio.litz

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONObject

internal interface IdentityRepository {
    fun loadOrCreate(): JSONObject

    fun remember(name: String, certificate: JSONArray, endpoint: String): JSONObject

    fun select(fingerprint: String): JSONObject

    fun saved(): List<SavedComputer>

    fun readSelected(): String?

    fun selectedFingerprint(): String?

    fun forget(fingerprint: String)

    fun updateAddress(fingerprint: String, endpoint: String): Boolean
}

internal class FakeIdentityRepository(
    private var identity: JSONObject =
        JSONObject().apply {
            put("registry_version", 1)
            put("known_desktops", JSONArray())
        }
) : IdentityRepository {
    private val computers = mutableListOf<SavedComputer>()

    override fun loadOrCreate(): JSONObject = identity

    override fun remember(name: String, certificate: JSONArray, endpoint: String): JSONObject {
        computers.removeAll { it.endpoint == endpoint }
        computers.add(SavedComputer(name, pin(certificate), endpoint))
        return identity
    }

    override fun select(fingerprint: String): JSONObject = identity

    override fun saved(): List<SavedComputer> = computers

    override fun readSelected(): String? = computers.firstOrNull()?.let { identity.toString() }

    override fun selectedFingerprint(): String? = computers.firstOrNull()?.fingerprint

    override fun forget(fingerprint: String) {
        computers.removeAll { it.fingerprint == fingerprint }
    }

    override fun updateAddress(fingerprint: String, endpoint: String): Boolean {
        val index = computers.indexOfFirst { it.fingerprint == fingerprint }
        return if (index >= 0) {
            computers[index] = computers[index].copy(endpoint = endpoint)
            true
        } else false
    }
}

/**
 * App-private, backup-excluded, atomic ciphertext. Keys never leave Keystore. TLS signing is native
 * software signing; this does NOT claim hardware TLS keys.
 */
internal class IdentityStore(
    context: Context,
    private val identityFactory: () -> String = { Native.createIdentity() },
    private val keyProvider: () -> SecretKey = ::identityKey,
) : IdentityRepository {
    companion object {
        private val monitor = Any()
    }

    private val file = AtomicFile(File(context.noBackupFilesDir, "identity.gcm"))

    private fun read(): String? {
        if (!file.baseFile.exists()) return null
        val bytes =
            file.openRead().use {
                require(it.channel.size() <= 32768) { "Invalid protected identity size" }
                it.readBytes()
            }
        require(bytes.size in 29..32768) { "Invalid protected identity" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            keyProvider(),
            GCMParameterSpec(128, bytes.copyOfRange(0, 12)),
        )
        return cipher.doFinal(bytes.copyOfRange(12, bytes.size)).toString(Charsets.UTF_8)
    }

    private fun write(identity: String) {
        require(identity.toByteArray().size <= 32000)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, keyProvider())
        val stream = file.startWrite()
        try {
            stream.write(cipher.iv)
            stream.write(cipher.doFinal(identity.toByteArray(Charsets.UTF_8)))
            file.finishWrite(stream)
        } catch (e: Exception) {
            file.failWrite(stream)
            throw e
        }
    }

    override fun loadOrCreate(): JSONObject =
        synchronized(monitor) {
            val existing = read()
            if (existing == null) {
                val created = JSONObject(identityFactory())
                check(!created.has("error")) { "Could not create phone identity" }
                created.put("registry_version", 1).put("known_desktops", JSONArray())
                write(created.toString())
                created
            } else {
                val identity = JSONObject(existing)
                check(identity.optInt("registry_version", -1) == 1) {
                    "Unsupported saved-device version"
                }
                check(identity.has("known_desktops")) { "Saved-device registry is missing" }
                identity
            }
        }

    override fun remember(name: String, certificate: JSONArray, endpoint: String): JSONObject =
        synchronized(monitor) {
            val identity = loadOrCreate()
            val saved = identity.getJSONArray("known_desktops")
            val pin = pin(certificate)
            for (i in 0 until saved.length()) {
                if (pin(saved.getJSONObject(i).getJSONArray("certificate")) == pin) {
                    saved.remove(i)
                    break
                }
            }
            check(saved.length() < 16) { "Saved computer list is full" }
            saved.put(
                JSONObject()
                    .put("name", name.take(64))
                    .put("certificate", certificate)
                    .put("endpoint", endpoint)
            )
            identity.put("desktop", certificate).put("endpoint", endpoint)
            write(identity.toString())
            identity
        }

    override fun select(fingerprint: String): JSONObject =
        synchronized(monitor) {
            val identity = loadOrCreate()
            val saved = identity.getJSONArray("known_desktops")
            val peer =
                (0 until saved.length())
                    .map { saved.getJSONObject(it) }
                    .firstOrNull { pin(it.getJSONArray("certificate")) == fingerprint }
                    ?: error("Saved computer unavailable")
            identity
                .put("desktop", peer.getJSONArray("certificate"))
                .put("endpoint", peer.getString("endpoint"))
            write(identity.toString())
            identity
        }

    override fun saved(): List<SavedComputer> {
        val saved = loadOrCreate().getJSONArray("known_desktops")
        return (0 until saved.length())
            .map { saved.getJSONObject(it) }
            .map {
                SavedComputer(
                    it.getString("name"),
                    pin(it.getJSONArray("certificate")),
                    it.getString("endpoint"),
                )
            }
    }

    override fun readSelected(): String? {
        val identity = loadOrCreate()
        return identity.toString().takeIf { (identity.optJSONArray("desktop")?.length() ?: 0) > 0 }
    }

    override fun selectedFingerprint(): String? =
        loadOrCreate().optJSONArray("desktop")?.takeIf { it.length() > 0 }?.let(::pin)

    override fun forget(fingerprint: String) =
        synchronized(monitor) {
            val identity = loadOrCreate()
            val saved = identity.getJSONArray("known_desktops")
            for (i in saved.length() - 1 downTo 0) {
                if (pin(saved.getJSONObject(i).getJSONArray("certificate")) == fingerprint)
                    saved.remove(i)
            }
            if (selectedFingerprint() == fingerprint) identity.put("desktop", JSONArray())
            write(identity.toString())
        }

    override fun updateAddress(fingerprint: String, endpoint: String): Boolean =
        synchronized(monitor) {
            val identity = loadOrCreate()
            val saved = identity.getJSONArray("known_desktops")
            var changed = false
            for (i in 0 until saved.length()) {
                val peer = saved.getJSONObject(i)
                if (
                    pin(peer.getJSONArray("certificate")) == fingerprint &&
                        peer.getString("endpoint") != endpoint
                ) {
                    peer.put("endpoint", endpoint)
                    changed = true
                }
            }
            if (changed) {
                if (selectedFingerprint() == fingerprint) identity.put("endpoint", endpoint)
                write(identity.toString())
            }
            changed
        }
}

internal fun pin(certificate: JSONArray): String {
    val bytes = ByteArray(certificate.length()) { certificate.getInt(it).toByte() }
    return MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") {
        "%02x".format(it.toInt() and 0xff)
    }
}

@androidx.compose.runtime.Immutable
internal data class SavedComputer(val name: String, val fingerprint: String, val endpoint: String)

private fun identityKey(): SecretKey {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey("litz.identity", null) as? SecretKey)?.let {
        return it
    }
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
        init(
            KeyGenParameterSpec.Builder(
                    "litz.identity",
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .build()
        )
        generateKey()
    }
}
