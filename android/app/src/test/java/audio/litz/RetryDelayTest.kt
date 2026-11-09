package audio.litz

import org.junit.Assert.assertEquals
import org.junit.Test

class RetryDelayTest {
    @Test
    fun aVisibleComputerIsRetriedSoonWithBackoff() {
        assertEquals(
            listOf(2_000L, 4_000L, 8_000L, 16_000L, 16_000L),
            (0..4).map { retryDelay(true, it) },
        )
    }

    @Test
    fun aComputerThatIsNotOnTheNetworkIsRetriedRarely() {
        assertEquals(60_000L, retryDelay(false, 0))
        assertEquals(60_000L, retryDelay(false, 10))
    }
}
