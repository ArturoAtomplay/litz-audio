package audio.litz

import audio.litz.native.mdnsHost
import java.net.DatagramPacket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.MulticastSocket

/** Look up a paired desktop on the tether interface, which Android NSD may not browse. */
internal object UsbMdns {
    fun resolve(fingerprint: String, port: Int, interfaces: List<UsbRoutes.Interface>): String? {
        if (!fingerprint.matches(Regex("[0-9a-f]{64}")) || port !in 1024..65535) return null
        val host = mdnsHost(fingerprint) ?: return null
        val question = query(host)
        val group = InetAddress.getByName("224.0.0.251")
        for (usb in interfaces) {
            runCatching {
                    MulticastSocket(null).use { socket ->
                        socket.reuseAddress = true
                        socket.bind(InetSocketAddress(usb.localAddress, 0))
                        socket.networkInterface = usb.network
                        socket.timeToLive = 1
                        socket.soTimeout = 600
                        repeat(2) {
                            socket.send(DatagramPacket(question, question.size, group, 5353))
                            val until = System.nanoTime() + 600_000_000L
                            while (System.nanoTime() < until) {
                                val buffer = ByteArray(1500)
                                val packet = DatagramPacket(buffer, buffer.size)
                                try {
                                    socket.receive(packet)
                                } catch (_: java.net.SocketTimeoutException) {
                                    break
                                }
                                val address = address(buffer, packet.length, host) ?: continue
                                val endpoint = "$address:$port"
                                if (usb.subnet.contains(endpoint)) return endpoint
                            }
                        }
                    }
                }
                .onFailure {}
        }
        return null
    }

    private fun query(host: String): ByteArray {
        val bytes = ArrayList<Byte>(64)
        repeat(12) { bytes.add(if (it == 5) 1 else 0) }
        host.split('.').forEach { label ->
            bytes.add(label.length.toByte())
            label.forEach { bytes.add(it.code.toByte()) }
        }
        bytes.add(0)
        bytes.add(0)
        bytes.add(1) // A record
        bytes.add(0x80.toByte())
        bytes.add(1) // request a unicast response to the bound socket
        return bytes.toByteArray()
    }

    internal fun address(packet: ByteArray, length: Int, host: String): String? {
        if (length < 12 || packet[2].toInt() and 0x80 == 0) return null
        fun word(at: Int): Int {
            if (at + 1 >= length) throw IndexOutOfBoundsException()
            return ((packet[at].toInt() and 255) shl 8) or (packet[at + 1].toInt() and 255)
        }
        fun name(start: Int): Pair<String, Int> {
            var offset = start
            var end = -1
            var jumps = 0
            val labels = mutableListOf<String>()
            while (true) {
                if (offset >= length || ++jumps > 32) throw IndexOutOfBoundsException()
                val size = packet[offset].toInt() and 255
                if (size and 0xc0 == 0xc0) {
                    if (end < 0) end = offset + 2
                    offset = ((size and 0x3f) shl 8) or (packet[offset + 1].toInt() and 255)
                    continue
                }
                if (size > 63 || offset + 1 + size > length) throw IndexOutOfBoundsException()
                offset++
                if (size == 0) return labels.joinToString(".") to (if (end >= 0) end else offset)
                labels.add(String(packet, offset, size, Charsets.US_ASCII))
                offset += size
            }
        }
        return runCatching {
                var offset = 12
                repeat(word(4)) {
                    offset = name(offset).second + 4
                    if (offset > length) throw IndexOutOfBoundsException()
                }
                val records = word(6) + word(8) + word(10)
                if (records > 128) return@runCatching null
                repeat(records) {
                    val (recordHost, next) = name(offset)
                    val type = word(next)
                    val recordClass = word(next + 2) and 0x7fff
                    val size = word(next + 8)
                    offset = next + 10
                    if (offset + size > length) throw IndexOutOfBoundsException()
                    if (
                        recordHost.equals(host, ignoreCase = true) &&
                            type == 1 &&
                            recordClass == 1 &&
                            size == 4
                    )
                        return@runCatching (0..3).joinToString(".") {
                            (packet[offset + it].toInt() and 255).toString()
                        }
                    offset += size
                }
                null
            }
            .getOrNull()
    }
}
