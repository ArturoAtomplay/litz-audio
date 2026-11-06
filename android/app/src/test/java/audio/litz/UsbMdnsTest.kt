package audio.litz

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class UsbMdnsTest {
    @Test
    fun reads_only_the_paired_hosts_ipv4_answer() {
        val host = "litz-ab3618f077e10f64.local"
        val packet = mutableListOf<Byte>()
        packet.addAll(byteArrayOf(0, 0, 0x84.toByte(), 0, 0, 0, 0, 1, 0, 0, 0, 0).toList())
        host.split('.').forEach { label ->
            packet.add(label.length.toByte())
            packet.addAll(label.toByteArray().toList())
        }
        packet.add(0)
        packet.addAll(byteArrayOf(0, 1, 0, 1, 0, 0, 0, 120, 0, 4, 10, -69, 24, -59).toList())
        val response = packet.toByteArray()
        assertEquals("10.187.24.197", UsbMdns.address(response, response.size, host))
        assertNull(UsbMdns.address(response, response.size, "another.local"))
        assertNull(UsbMdns.address(response, response.size - 1, host))
    }
}
