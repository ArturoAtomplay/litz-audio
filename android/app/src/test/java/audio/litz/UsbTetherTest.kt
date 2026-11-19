package audio.litz

import android.app.Application
import android.content.ComponentName
import android.content.Intent
import android.content.IntentFilter
import android.provider.Settings
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [33])
class UsbTetherTest {
    private val app = ApplicationProvider.getApplicationContext<Application>()
    private val packages = shadowOf(app.packageManager)

    @Before
    fun failOnMissingActivities() {
        shadowOf(app).checkActivities(true)
    }

    private fun screen(component: ComponentName, action: String? = null) {
        packages.addActivityIfNotPresent(component)
        if (action != null)
            packages.addIntentFilterForActivity(
                component,
                IntentFilter(action).apply { addCategory(Intent.CATEGORY_DEFAULT) },
            )
    }

    private fun started(): Intent? = shadowOf(app).nextStartedActivity

    @Test
    fun opensTheTetherActionWhereTheSettingsAppHandlesIt() {
        val activity =
            ComponentName(
                "com.android.settings",
                "com.android.settings.Settings\$TetherSettingsActivity",
            )
        screen(activity, "android.settings.TETHER_SETTINGS")
        assertTrue(UsbTether.open(app))
        assertEquals("android.settings.TETHER_SETTINGS", started()?.action)
    }

    @Test
    fun opensTheLegacyAliasWithoutTheAction() {
        val alias = ComponentName("com.android.settings", "com.android.settings.TetherSettings")
        screen(alias)
        assertTrue(UsbTether.open(app))
        assertEquals(alias, started()?.component)
    }

    @Test
    fun fallsBackToNetworkSettingsAndAsksTheUserToFindIt() {
        screen(
            ComponentName("com.android.settings", "com.android.settings.Settings\$NetworkActivity"),
            Settings.ACTION_WIRELESS_SETTINGS,
        )
        assertFalse(UsbTether.open(app))
        assertEquals(Settings.ACTION_WIRELESS_SETTINGS, started()?.action)
    }

    @Test
    fun survivesAPhoneWithNoSettingsScreens() {
        assertFalse(UsbTether.open(app))
        assertNull(started())
    }
}
