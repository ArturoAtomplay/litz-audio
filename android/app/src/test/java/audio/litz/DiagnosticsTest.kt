package audio.litz

import android.content.Context
import android.content.res.Configuration
import androidx.test.core.app.ApplicationProvider
import java.util.Locale
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class DiagnosticsTest {
    private val spanish: Context =
        ApplicationProvider.getApplicationContext<Context>().let {
            it.createConfigurationContext(
                Configuration(it.resources.configuration).apply { setLocale(Locale("es")) }
            )
        }

    @Test
    fun theScreenFollowsThePhoneButTheCopiedReportIsEnglish() {
        val session = Snapshot(SessionStatus(status = status()))
        val shown = diagnosticText(diagnostics(spanish, UiState(), session))
        val copied = diagnosticText(diagnostics(spanish.inEnglish(), UiState(), session))
        assertTrue(shown, shown.contains("Conexión"))
        assertTrue(copied, copied.contains("Connection\n  Status: Connected"))
    }
}
