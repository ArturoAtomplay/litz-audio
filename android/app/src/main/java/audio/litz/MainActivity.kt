package audio.litz

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Ads.start(this)
        setContent {
            LitzTheme {
                val vm: SessionViewModel = viewModel()
                val ui by vm.state.collectAsStateWithLifecycle()
                val owner = LocalLifecycleOwner.current
                DisposableEffect(owner) {
                    val observer = LifecycleEventObserver { _, event ->
                        if (event == Lifecycle.Event.ON_START) vm.visible(true)
                        if (event == Lifecycle.Event.ON_STOP) vm.visible(false)
                    }
                    owner.lifecycle.addObserver(observer)
                    vm.visible(owner.lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED))
                    onDispose {
                        owner.lifecycle.removeObserver(observer)
                        vm.visible(false)
                    }
                }
                var pending by remember { mutableStateOf<Settings?>(null) }
                var connectPending by remember { mutableStateOf(false) }
                val discoveryPermission =
                    rememberLauncherForActivityResult(
                        ActivityResultContracts.RequestPermission()
                    ) { granted ->
                        if (granted) vm.startDiscovery()
                        else vm.message("Allow local network access to find nearby computers.")
                    }
                LaunchedEffect(Unit) {
                    if (
                        Build.VERSION.SDK_INT >= 37 &&
                            checkSelfPermission("android.permission.ACCESS_LOCAL_NETWORK") !=
                                PackageManager.PERMISSION_GRANTED
                    )
                        discoveryPermission.launch("android.permission.ACCESS_LOCAL_NETWORK")
                    else vm.startDiscovery()
                }
                val permissions =
                    rememberLauncherForActivityResult(
                        ActivityResultContracts.RequestMultiplePermissions()
                    ) {
                        val next = pending
                        pending = null
                        val lan =
                            Build.VERSION.SDK_INT < 37 ||
                                checkSelfPermission("android.permission.ACCESS_LOCAL_NETWORK") ==
                                    PackageManager.PERMISSION_GRANTED
                        val mic =
                            next?.mic != true ||
                                checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
                                    PackageManager.PERMISSION_GRANTED
                        if (lan && mic) {
                            if (connectPending) vm.connect() else next?.let(vm::startAuthorized)
                        } else
                            vm.message(
                                "Permission denied. Allow local network access and, for microphone mode, microphone access in Android settings, then try again."
                            )
                        connectPending = false
                    }
                fun authorize(settings: Settings?) {
                    if (settings != null && !settings.mic && !settings.playback) {
                        vm.stop()
                        return
                    }
                    val needed = buildList {
                        if (
                            settings?.mic == true &&
                                checkSelfPermission(Manifest.permission.RECORD_AUDIO) !=
                                    PackageManager.PERMISSION_GRANTED
                        )
                            add(Manifest.permission.RECORD_AUDIO)
                        if (
                            Build.VERSION.SDK_INT >= 37 &&
                                checkSelfPermission("android.permission.ACCESS_LOCAL_NETWORK") !=
                                    PackageManager.PERMISSION_GRANTED
                        )
                            add("android.permission.ACCESS_LOCAL_NETWORK")
                        if (
                            settings != null &&
                                Build.VERSION.SDK_INT >= 33 &&
                                checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
                                    PackageManager.PERMISSION_GRANTED
                        )
                            add(Manifest.permission.POST_NOTIFICATIONS)
                    }
                    if (needed.isEmpty()) {
                        if (settings == null) vm.connect() else vm.startAuthorized(settings)
                    } else {
                        pending = settings
                        connectPending = settings == null
                        permissions.launch(needed.toTypedArray())
                    }
                }
                DevicesApp(
                    ui,
                    pending != null || connectPending,
                    start = { authorize(it) },
                    stop = vm::stop,
                    connect = { authorize(null) },
                    disconnect = vm::disconnect,
                    pair = vm::pair,
                    cancelPair = vm::cancelPair,
                    select = vm::select,
                    refresh = {
                        if (
                            Build.VERSION.SDK_INT >= 37 &&
                                checkSelfPermission("android.permission.ACCESS_LOCAL_NETWORK") !=
                                    PackageManager.PERMISSION_GRANTED
                        )
                            discoveryPermission.launch("android.permission.ACCESS_LOCAL_NETWORK")
                        else vm.refreshDiscovery()
                    },
                    edit = vm::edit,
                    apply = vm::applyGains,
                    forget = vm::forget,
                    route = vm::route,
                    refreshRoutes = vm::refreshRoutes,
                    voice = vm::voice,
                )
            }
        }
    }
}
