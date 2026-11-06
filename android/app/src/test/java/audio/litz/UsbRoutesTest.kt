package audio.litz

import org.junit.Assert.assertEquals
import org.junit.Test

class UsbRoutesTest {
    @Test
    fun prefers_matching_usb_address_and_preserves_fallbacks() {
        val wifi = "192.168.1.10:47165"
        val usb = "192.168.42.2:47165"
        val computer = NearbyComputer("Studio", "a".repeat(64), wifi, listOf(wifi, usb))
        val subnet = UsbSubnet(0xc0a82a81.toInt(), 24)
        assertEquals(
            listOf(usb, wifi),
            UsbRoutes.prefer(listOf(computer), listOf(subnet)).single().endpoints,
        )
        assertEquals(usb, UsbRoutes.prefer(listOf(computer), listOf(subnet)).single().endpoint)
        assertEquals(wifi, UsbRoutes.prefer(listOf(computer), emptyList()).single().endpoint)
    }

    @Test
    fun subnet_match_rejects_unrelated_or_non_ipv4_endpoints() {
        val subnet = UsbSubnet(0xc0a82a81.toInt(), 24)
        assertEquals(true, subnet.contains("192.168.42.2:47165"))
        assertEquals(false, subnet.contains("192.168.43.2:47165"))
        assertEquals(false, subnet.contains("[fe80::1]:47165"))
    }
}
