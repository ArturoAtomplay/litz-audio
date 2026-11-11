package audio.litz

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class IdentityStoreTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")

    private fun store() = IdentityStore(context, keyProvider = { key })

    private fun file() = File(context.noBackupFilesDir, "identity.gcm")

    @Test
    fun concurrent_registry_operations_preserve_both_pins_and_selection() {
        file().delete()
        val a = store()
        val b = store()
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(2)
        try {
            val one =
                pool.submit {
                    start.await()
                    a.remember("One", byteArrayOf(1, 2, 3), "192.0.2.1:47165")
                }
            val two =
                pool.submit {
                    start.await()
                    b.remember("Two", byteArrayOf(4, 5, 6), "192.0.2.2:47165")
                }
            start.countDown()
            one.get(5, TimeUnit.SECONDS)
            two.get(5, TimeUnit.SECONDS)
            val peers = a.saved()
            assertEquals(2, peers.size)
            val pin = peers.first { it.name == "One" }.fingerprint
            val selection = pool.submit { a.select(pin) }
            val update = pool.submit { b.updateAddress(pin, "192.0.2.3:47165") }
            selection.get(5, TimeUnit.SECONDS)
            update.get(5, TimeUnit.SECONDS)
            assertEquals(pin, b.selectedFingerprint())
            assertEquals("192.0.2.3:47165", b.phone().endpoint)
            b.forget(pin)
            assertNull(a.selectedFingerprint())
            assertEquals("Two", a.saved().single().name)
            assertFalse(file().readText().contains("192.0.2"))
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun a_fresh_phone_has_an_identity_and_no_computers() {
        file().delete()
        val phone = store().phone()
        assertTrue(phone.certificate.isNotEmpty() && phone.privateKey.isNotEmpty())
        assertTrue(store().saved().isEmpty())
        assertNull(store().readSelected())
        assertArrayEquals(phone.certificate, store().phone().certificate)
    }

    @Test
    fun a_file_saved_by_0_1_0_keeps_its_identity_and_computers() {
        val saved =
            """{"desktop":[255],"certificate":[1,200],"private_key":[2],"endpoint":"192.0.2.1:47165","registry_version":1,"known_desktops":[{"name":"One","certificate":[255],"endpoint":"192.0.2.1:47165"}]}"""
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key)
        file().writeBytes(cipher.iv + cipher.doFinal(saved.toByteArray()))
        val phone = store().readSelected()!!
        assertArrayEquals(byteArrayOf(1, -56), phone.certificate)
        assertArrayEquals(byteArrayOf(-1), phone.desktop)
        assertEquals("192.0.2.1:47165", phone.endpoint)
        assertEquals("One", store().saved().single().name)
    }
}
