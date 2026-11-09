package audio.litz

import android.content.Context
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.google.android.gms.ads.AdListener
import com.google.android.gms.ads.AdRequest
import com.google.android.gms.ads.AdSize
import com.google.android.gms.ads.AdView
import com.google.android.gms.ads.MobileAds
import com.google.android.ump.ConsentInformation
import com.google.android.ump.ConsentRequestParameters
import com.google.android.ump.UserMessagingPlatform
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow

private const val MAX_BANNER_HEIGHT = 64

internal object Ads {
    private val requested = AtomicBoolean(false)
    private val initialized = AtomicBoolean(false)
    private val _ready = MutableStateFlow(false)
    private val _privacyOptions = MutableStateFlow(false)
    val ready = _ready.asStateFlow()
    val privacyOptions = _privacyOptions.asStateFlow()

    fun start(activity: ComponentActivity) {
        if (!requested.compareAndSet(false, true)) return
        val consent = UserMessagingPlatform.getConsentInformation(activity)
        consent.requestConsentInfoUpdate(
            activity,
            ConsentRequestParameters.Builder().build(),
            {
                UserMessagingPlatform.loadAndShowConsentFormIfRequired(activity) {
                    update(activity.applicationContext, consent)
                }
            },
            { update(activity.applicationContext, consent) },
        )
        update(activity.applicationContext, consent)
    }

    private fun update(context: Context, consent: ConsentInformation) {
        _privacyOptions.value =
            consent.privacyOptionsRequirementStatus ==
                ConsentInformation.PrivacyOptionsRequirementStatus.REQUIRED
        if (!consent.canRequestAds() || !initialized.compareAndSet(false, true)) return
        Thread { MobileAds.initialize(context) { _ready.value = true } }.start()
    }
}

@Composable
internal fun AdBanner() {
    val ready by Ads.ready.collectAsStateWithLifecycle()
    if (!ready) return
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val context = LocalContext.current
        val width = maxWidth.value.toInt()
        var loadedHeight by remember(width) { mutableIntStateOf(0) }
        val surface = MaterialTheme.colorScheme.surface
        val view =
            remember(width) {
                AdView(context).apply {
                    adUnitId = BuildConfig.ADMOB_BANNER_ID
                    setBackgroundColor(surface.toArgb())
                    setAdSize(AdSize.getInlineAdaptiveBannerAdSize(width, MAX_BANNER_HEIGHT))
                    adListener =
                        object : AdListener() {
                            override fun onAdLoaded() {
                                loadedHeight = adSize?.height ?: 0
                            }
                        }
                    loadAd(AdRequest.Builder().build())
                }
            }
        val lifecycle = LocalLifecycleOwner.current.lifecycle
        DisposableEffect(view, lifecycle) {
            val observer = LifecycleEventObserver { _, event ->
                if (event == Lifecycle.Event.ON_RESUME) view.resume()
                if (event == Lifecycle.Event.ON_PAUSE) view.pause()
            }
            lifecycle.addObserver(observer)
            onDispose {
                lifecycle.removeObserver(observer)
                view.destroy()
            }
        }
        val height by animateDpAsState(loadedHeight.dp, label = "banner")
        val loaded = loadedHeight > 0
        Column(
            Modifier.fillMaxWidth()
                .background(if (loaded) MaterialTheme.colorScheme.surface else Color.Transparent)
                .navigationBarsPadding()
        ) {
            if (loaded) HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Box(
                Modifier.fillMaxWidth().height(height).clipToBounds(),
                contentAlignment = Alignment.TopCenter,
            ) {
                AndroidView(
                    { view },
                    Modifier.requiredSize(
                        width.dp,
                        (if (loaded) loadedHeight else MAX_BANNER_HEIGHT).dp,
                    ),
                )
            }
        }
    }
}

@Composable
internal fun StoreOptions() {
    val required by Ads.privacyOptions.collectAsStateWithLifecycle()
    val activity = LocalActivity.current
    if (!required || activity == null) return
    SettingRow(
        "Ad privacy choices",
        onClick = { UserMessagingPlatform.showPrivacyOptionsForm(activity) {} },
    )
}
