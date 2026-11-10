package audio.litz

import android.content.Context
import android.content.res.Configuration
import androidx.test.core.app.ApplicationProvider
import audio.litz.native.Code
import audio.litz.native.Failure
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class FailureTextTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    private fun inLanguage(tag: String): Context =
        context.createConfigurationContext(
            Configuration(context.resources.configuration).apply {
                setLocale(Locale.forLanguageTag(tag))
            }
        )

    @Test
    fun aRefusalDependsOnTheDirectionItBelongsTo() {
        val conflict = Failure.ComputerRefused(Code.ROUTE_CONFLICT)
        assertEquals(R.string.failure_no_virtual_microphone, failureText(conflict, 0).id)
        assertEquals(R.string.failure_capture_unavailable, failureText(conflict, 1).id)
        assertEquals(
            UiText(R.string.failure_computer_unsupported),
            failureText(Failure.ComputerRefused(Code.UNSUPPORTED)),
        )
    }

    @Test
    fun theOwnerNameTravelsIntoTheTextInEitherLanguage() {
        val text = failureText(Failure.InUse("Pixel 8"))
        assertEquals(
            "En uso por Pixel 8. ",
            text.resolve(inLanguage("es")).substringBefore("Activa"),
        )
        assertEquals("In use by Pixel 8. ", text.resolve(inLanguage("en")).substringBefore("Turn"))
        assertEquals(R.string.failure_in_use_another_phone, failureText(Failure.InUse(null)).id)
    }

    @Test
    fun anInternalFailureKeepsItsDetail() {
        val text = failureText(Failure.Internal("epoch exhausted"))
        assertEquals(UiText(R.string.failure_internal, listOf("epoch exhausted")), text)
        assertNotEquals(text.resolve(inLanguage("en")), text.resolve(inLanguage("es")))
    }
}
