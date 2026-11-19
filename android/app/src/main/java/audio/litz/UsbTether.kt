package audio.litz

import android.content.*
import android.provider.Settings

internal object UsbTether {
    private const val SETTINGS = "com.android.settings"

    private val tetherScreens =
        listOf(
            Intent("android.settings.TETHER_SETTINGS"),
            Intent().setClassName(SETTINGS, "$SETTINGS.TetherSettings"),
            Intent().setClassName(SETTINGS, "$SETTINGS.Settings\$TetherSettingsActivity"),
        )
    private val fallbackScreens =
        listOf(Intent(Settings.ACTION_WIRELESS_SETTINGS), Intent(Settings.ACTION_SETTINGS))

    /** Whether the tethering screen itself opened rather than a screen leading to it. */
    fun open(context: Context): Boolean {
        val start = { screen: Intent ->
            try {
                context.startActivity(Intent(screen).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                true
            } catch (_: ActivityNotFoundException) {
                false
            } catch (_: SecurityException) {
                false
            }
        }
        if (tetherScreens.any(start)) return true
        fallbackScreens.any(start)
        return false
    }
}
