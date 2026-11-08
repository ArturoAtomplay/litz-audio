package audio.litz

import java.net.Inet4Address
import java.net.NetworkInterface

internal data class UsbSubnet(val address: Int, val prefixLength: Int) {
    fun contains(endpoint: String): Boolean {
        val host = endpoint.substringBeforeLast(':', "")
        val octets = host.split('.')
        if (octets.size != 4) return false
        val value =
            octets.fold(0) { result, part ->
                val octet = part.toIntOrNull()?.takeIf { it in 0..255 } ?: return false
                (result shl 8) or octet
            }
        val mask = if (prefixLength == 0) 0 else -1 shl (32 - prefixLength)
        return (value and mask) == (address and mask)
    }
}

internal object UsbRoutes {
    data class Interface(
        val network: NetworkInterface,
        val localAddress: Inet4Address,
        val subnet: UsbSubnet,
    )

    fun interfaces(): List<Interface> =
        runCatching {
                NetworkInterface.getNetworkInterfaces()
                    .toList()
                    .filter {
                        it.isUp &&
                            !it.isLoopback &&
                            (it.name.startsWith("rndis") ||
                                it.name.startsWith("usb") ||
                                it.name.startsWith("ncm"))
                    }
                    .flatMap { network ->
                        network.interfaceAddresses.mapNotNull { interfaceAddress ->
                            val address =
                                interfaceAddress.address as? Inet4Address ?: return@mapNotNull null
                            val prefix = interfaceAddress.networkPrefixLength.toInt()
                            if (prefix !in 1..32) return@mapNotNull null
                            Interface(
                                network,
                                address,
                                UsbSubnet(
                                    address.address.fold(0) { value, byte ->
                                        (value shl 8) or (byte.toInt() and 255)
                                    },
                                    prefix,
                                ),
                            )
                        }
                    }
            }
            .getOrDefault(emptyList())

    fun subnets(): List<UsbSubnet> = interfaces().map { it.subnet }

    fun prefer(computers: List<NearbyComputer>, subnets: List<UsbSubnet>): List<NearbyComputer> =
        computers.map { computer ->
            val endpoints =
                computer.endpoints.distinct().sortedBy { endpoint ->
                    if (subnets.any { it.contains(endpoint) }) 0 else 1
                }
            computer.copy(
                endpoint = endpoints.firstOrNull() ?: computer.endpoint,
                endpoints = endpoints,
            )
        }
}
