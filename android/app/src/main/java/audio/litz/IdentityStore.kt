package audio.litz

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import audio.litz.native.Identity
import audio.litz.native.SavedComputer
import audio.litz.native.registryCreate
import audio.litz.native.registryForget
import audio.litz.native.registryPhone
import audio.litz.native.registryRemember
import audio.litz.native.registrySaved
import audio.litz.native.registrySelect
import audio.litz.native.registrySelected
import audio.litz.native.registryUpdateAddress
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * App-private, backup-excluded, atomic ciphertext. Keys never leave Keystore. TLS signing is native
 * software signing; this does NOT claim hardware TLS keys. The plaintext is the native engine's
 * registry, which only the engine reads and changes.
 */
internal class IdentityStore(
    context: Context,
    private val keyProvider: () -> SecretKey = ::identityKey,
) {
    companion object {
        private val monitor = Any()
    }

    private val file = AtomicFile(File(context.noBackupFilesDir, "identity.gcm"))

    private fun read(): ByteArray? {
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
        return cipher.doFinal(bytes.copyOfRange(12, bytes.size))
    }

    private fun write(registry: ByteArray) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, keyProvider())
        val stream = file.startWrite()
        try {
            stream.write(cipher.iv)
            stream.write(cipher.doFinal(registry))
            file.finishWrite(stream)
        } catch (e: Exception) {
            file.failWrite(stream)
            throw e
        }
    }

    private fun registry(): ByteArray =
        synchronized(monitor) { read() ?: registryCreate().also(::write) }

    fun remember(name: String, certificate: ByteArray, endpoint: String) =
        synchronized(monitor) { write(registryRemember(registry(), name, certificate, endpoint)) }

    fun select(fingerprint: String) =
        synchronized(monitor) { write(registrySelect(registry(), fingerprint)) }

    fun saved(): List<SavedComputer> = registrySaved(registry())

    fun phone(): Identity = registryPhone(registry())

    fun readSelected(): Identity? = phone().takeIf { it.desktop.isNotEmpty() }

    fun selectedFingerprint(): String? = registrySelected(registry())

    fun forget(fingerprint: String) =
        synchronized(monitor) { write(registryForget(registry(), fingerprint)) }

    fun updateAddress(fingerprint: String, endpoint: String): Boolean =
        synchronized(monitor) {
            registryUpdateAddress(registry(), fingerprint, endpoint)?.also(::write) != null
        }
}

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
