package audio.litz

import org.junit.Assert.assertEquals
import org.junit.Test

class DiagnosticTextTest {
    @Test
    fun copiedReportListsEachSectionWithIndentedRows() {
        val text =
            diagnosticText(
                listOf(
                    DiagnosticSection("App", listOf("Version" to "0.1.0 (foss)")),
                    DiagnosticSection("Listen", listOf("Status" to "On", "Late packets" to "0")),
                )
            )
        assertEquals("App\n  Version: 0.1.0 (foss)\nListen\n  Status: On\n  Late packets: 0", text)
    }
}
