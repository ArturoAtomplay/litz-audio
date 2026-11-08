package audio.litz

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.crypto.spec.SecretKeySpec
import org.json.JSONArray
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

    private fun store(
        initial: String =
            """{"desktop":[],"certificate":[1],"private_key":[2],"endpoint":"127.0.0.1:47165"}"""
    ) = IdentityStore(context, identityFactory = { initial }, keyProvider = { key })

    @Test
    fun concurrent_registry_operations_preserve_both_pins_and_selection() {
        File(context.noBackupFilesDir, "identity.gcm").delete()
        val a = store()
        val b = store()
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(2)
        try {
            val one =
                pool.submit {
                    start.await()
                    a.remember("One", JSONArray("[1,2,3]"), "192.0.2.1:47165")
                }
            val two =
                pool.submit {
                    start.await()
                    b.remember("Two", JSONArray("[4,5,6]"), "192.0.2.2:47165")
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
            assertEquals("192.0.2.3:47165", b.loadOrCreate().getString("endpoint"))
            b.forget(pin)
            assertNull(a.selectedFingerprint())
            assertEquals("Two", a.saved().single().name)
            assertFalse(
                File(context.noBackupFilesDir, "identity.gcm").readText().contains("192.0.2")
            )
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun fresh_identity_carries_the_current_registry_shape() {
        File(context.noBackupFilesDir, "identity.gcm").delete()
        val identity = store().loadOrCreate()
        assertEquals(1, identity.getInt("registry_version"))
        assertEquals(0, identity.getJSONArray("known_desktops").length())
    }
}
