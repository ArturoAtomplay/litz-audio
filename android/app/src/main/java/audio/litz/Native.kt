package audio.litz

internal object Native {
    init {
        System.loadLibrary("litz_android")
    }

    external fun createIdentity(): String

    external fun pairStart(identity: String, endpoint: String, expected: String, name: String): Long

    external fun pairStatus(handle: Long): String

    external fun pairCancel(handle: Long)

    external fun pairClose(handle: Long)

    external fun open(identity: String): Long

    external fun command(handle: Long, command: String): String

    external fun cancel(handle: Long)

    external fun close(handle: Long)
}
