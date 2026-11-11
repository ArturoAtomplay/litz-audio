package audio.litz

import android.content.ClipData
import android.content.ClipboardManager
import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import audio.litz.native.DesktopRoutes
import audio.litz.native.Route
import audio.litz.native.SessionState
import audio.litz.native.Settings
import audio.litz.native.VoiceSettings
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal val microphoneSources =
    listOf(
        MicrophoneSource(0, R.string.source_automatic, R.string.source_automatic_description),
        MicrophoneSource(1, R.string.source_standard, R.string.source_standard_description),
        MicrophoneSource(7, R.string.source_calls, R.string.source_calls_description),
        MicrophoneSource(6, R.string.source_speech, R.string.source_speech_description),
        MicrophoneSource(10, R.string.source_performance, R.string.source_performance_description),
        MicrophoneSource(5, R.string.source_camera, R.string.source_camera_description),
        MicrophoneSource(9, R.string.source_raw, R.string.source_raw_description),
    )

internal data class MicrophoneSource(
    val value: Int,
    @StringRes val title: Int,
    @StringRes val description: Int,
)

internal fun manualEndpoint(input: String): String? {
    val parts = input.trim().split(':')
    if (parts.size !in 1..2) return null
    val octets = parts[0].split('.')
    if (
        octets.size != 4 ||
            octets.any {
                it.isEmpty() ||
                    it.length > 3 ||
                    !it.all(Char::isDigit) ||
                    it.toIntOrNull()?.let { value -> value in 0..255 } != true
            }
    )
        return null
    val first = octets[0].toInt()
    if (first == 0 || first >= 224 || octets.all { it.toInt() == 0 }) return null
    val port = if (parts.size == 2) parts[1].toIntOrNull() else 47165
    if (port == null || port !in 1024..65535 || (parts.size == 2 && parts[1].any { !it.isDigit() }))
        return null
    return "${octets.joinToString(".") { it.toInt().toString() }}:$port"
}

private enum class Screen {
    Home,
    Computers,
    Settings,
}

private const val SOURCE_URL = "https://github.com/ArturoAtomplay/litz-audio"

@Composable
internal fun DevicesApp(
    ui: UiState,
    permissionPending: Boolean,
    start: (Settings) -> Unit,
    stop: () -> Unit,
    connect: () -> Unit,
    pair: (NearbyComputer) -> Unit,
    confirmPair: () -> Unit,
    cancelPair: () -> Unit,
    select: (SavedComputer) -> Unit,
    refresh: () -> Unit,
    edit: (Settings) -> Unit,
    apply: () -> Unit,
    forget: () -> Unit,
    route: (ULong, Route) -> Unit = { _, _ -> },
    refreshRoutes: () -> Unit = {},
    voice: (ULong, VoiceSettings) -> Unit = { _, _ -> },
) {
    var screen by rememberSaveable { mutableStateOf(Screen.Home) }
    BackHandler(enabled = screen != Screen.Home) { screen = Screen.Home }
    var lastSelected by remember { mutableStateOf(ui.selected) }
    LaunchedEffect(ui.selected) {
        if (ui.selected != lastSelected) {
            lastSelected = ui.selected
            if (ui.selected != null) screen = Screen.Home
        }
    }
    val status = remember(ui.session) { Snapshot(ui.session) }
    val busy = ui.session.audioPending || ui.pairing != null || permissionPending
    val selected = ui.saved.firstOrNull { it.fingerprint == ui.selected }
    val error = ui.session.error?.takeUnless { ui.quiet && !status.connected }
    val scrolls = Screen.entries.associateWith { rememberScrollState() }
    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        topBar = {
            Box(Modifier.fillMaxWidth().statusBarsPadding(), contentAlignment = Alignment.Center) {
                Row(
                    Modifier.widthIn(max = 600.dp)
                        .fillMaxWidth()
                        .padding(horizontal = 20.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        stringResource(
                            when (screen) {
                                Screen.Home -> R.string.app_short_name
                                Screen.Computers -> R.string.screen_computers
                                Screen.Settings -> R.string.screen_settings
                            }
                        ),
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    if (screen == Screen.Home)
                        TextButton(onClick = { screen = Screen.Settings }) {
                            Text(stringResource(R.string.screen_settings))
                        }
                    else
                        TextButton(onClick = { screen = Screen.Home }) {
                            Text(stringResource(R.string.action_done))
                        }
                }
            }
        },
        bottomBar = { AdBanner() },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.TopCenter) {
            Column(
                Modifier.widthIn(max = 600.dp)
                    .fillMaxWidth()
                    .fillMaxHeight()
                    .verticalScroll(scrolls.getValue(screen))
                    .padding(horizontal = 20.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                ui.message?.takeUnless { it == ui.session.error }?.let { Notice(it.resolve()) }
                error?.let { Notice(it.resolve(), error = true) }
                when (screen) {
                    Screen.Home ->
                        if (selected == null) {
                            Text(
                                stringResource(R.string.home_add_title),
                                style = MaterialTheme.typography.titleLarge,
                            )
                            Text(
                                stringResource(R.string.home_add_body),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            AddComputer(ui, busy, pair, confirmPair, cancelPair, refresh)
                        } else {
                            ComputerCard(
                                selected,
                                ui,
                                status,
                                busy,
                                connect = connect,
                                change = { screen = Screen.Computers },
                                forget = forget,
                                pairAgain = pair,
                            )
                            MicrophoneCard(ui.settings, status, ui.session, busy, start)
                            ListenCard(
                                ui.settings,
                                status,
                                ui.session,
                                busy,
                                start,
                                edit,
                                apply,
                                route,
                                refreshRoutes,
                            )
                        }
                    Screen.Computers -> {
                        val others = ui.saved.filter { it.fingerprint != ui.selected }
                        if (selected != null || others.isNotEmpty()) {
                            Text(
                                stringResource(R.string.computers_yours),
                                style = MaterialTheme.typography.titleLarge,
                            )
                            selected?.let {
                                OutlinedCard(
                                    Modifier.fillMaxWidth(),
                                    shape = RoundedCornerShape(24.dp),
                                ) {
                                    SettingValue(
                                        it.name,
                                        stringResource(R.string.computer_using_now),
                                        enabled = false,
                                    )
                                }
                            }
                            others.forEach { computer ->
                                ChoiceCard(
                                    computer.name,
                                    stringResource(R.string.computer_tap_to_switch),
                                    enabled = !busy,
                                ) {
                                    select(computer)
                                }
                            }
                        }
                        AddComputer(ui, busy, pair, confirmPair, cancelPair, refresh)
                    }
                    Screen.Settings -> SettingsScreen(ui, status, busy, edit, apply, voice)
                }
            }
        }
    }
}

@Composable
private fun ComputerCard(
    computer: SavedComputer,
    ui: UiState,
    status: Snapshot,
    busy: Boolean,
    connect: () -> Unit,
    change: () -> Unit,
    forget: () -> Unit,
    pairAgain: (NearbyComputer) -> Unit,
) {
    val nearby = ui.nearby.firstOrNull { it.fingerprint == computer.fingerprint }
    val connecting =
        status.state == SessionState.RECONNECTING ||
            (ui.session.pending && !status.connected && (!ui.quiet || nearby != null))
    val usb by
        produceState(false, status.endpoint) {
            value =
                status.endpoint?.let { endpoint ->
                    withContext(Dispatchers.IO) {
                        UsbRoutes.subnets().any { it.contains(endpoint) }
                    }
                } == true
        }
    val (line, color) =
        when {
            status.revoked && !status.connected ->
                stringResource(R.string.status_revoked) to MaterialTheme.colorScheme.error
            status.inUse ->
                stringResource(R.string.status_in_use, status.owner.orEmpty()) to
                    MaterialTheme.colorScheme.tertiary
            status.connected ->
                stringResource(
                    if (usb) R.string.status_connected_usb else R.string.status_connected
                ) to MaterialTheme.colorScheme.primary
            connecting ->
                stringResource(R.string.status_connecting) to MaterialTheme.colorScheme.tertiary
            nearby != null && ui.session.error != null ->
                stringResource(R.string.status_not_answering) to MaterialTheme.colorScheme.outline
            else -> stringResource(R.string.status_offline) to MaterialTheme.colorScheme.outline
        }
    var menu by remember { mutableStateOf(false) }
    var confirmForget by remember { mutableStateOf(false) }
    Card(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(28.dp),
        colors =
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer),
    ) {
        Column(Modifier.padding(start = 20.dp, end = 8.dp, top = 12.dp, bottom = 16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    computer.name,
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleLarge,
                )
                Box {
                    IconButton(onClick = { menu = true }) {
                        Icon(
                            painterResource(R.drawable.ic_more),
                            contentDescription = stringResource(R.string.action_more),
                        )
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.computer_change)) },
                            enabled = !busy,
                            onClick = {
                                menu = false
                                change()
                            },
                        )
                        DropdownMenuItem(
                            text = {
                                Text(
                                    stringResource(R.string.computer_forget),
                                    color = MaterialTheme.colorScheme.error,
                                )
                            },
                            enabled = !busy,
                            onClick = {
                                menu = false
                                confirmForget = true
                            },
                        )
                    }
                }
            }
            Row(
                Modifier.padding(end = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Canvas(Modifier.size(8.dp)) { drawCircle(color) }
                Text(line, style = MaterialTheme.typography.bodyMedium)
            }
            if (!status.connected && !connecting) {
                Text(
                    when {
                        status.revoked -> stringResource(R.string.hint_pair_again)
                        nearby != null -> stringResource(R.string.hint_firewall)
                        else -> stringResource(R.string.hint_connects_by_itself, computer.name)
                    },
                    Modifier.padding(top = 8.dp, end = 12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Row(Modifier.padding(top = 12.dp)) {
                    if (status.revoked)
                        nearby?.let {
                            Button(onClick = { pairAgain(it) }, enabled = !busy) {
                                Text(stringResource(R.string.action_pair_again))
                            }
                        }
                    else
                        Button(onClick = connect, enabled = !busy) {
                            Text(stringResource(R.string.action_try_again))
                        }
                }
            }
        }
    }
    if (confirmForget)
        AlertDialog(
            containerColor = MaterialTheme.colorScheme.surface,
            onDismissRequest = { confirmForget = false },
            title = { Text(stringResource(R.string.computer_forget_title, computer.name)) },
            text = { Text(stringResource(R.string.computer_forget_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmForget = false
                        forget()
                    }
                ) {
                    Text(
                        stringResource(R.string.action_forget),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmForget = false }) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
}

@Composable
private fun AddComputer(
    ui: UiState,
    busy: Boolean,
    pair: (NearbyComputer) -> Unit,
    confirmPair: () -> Unit,
    cancelPair: () -> Unit,
    refresh: () -> Unit,
) {
    var candidate by remember { mutableStateOf<NearbyComputer?>(null) }
    var manualDialog by remember { mutableStateOf(false) }
    if (ui.pairing != null) {
        Card(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                LitzBars(Modifier.size(width = 54.dp, height = 40.dp))
                Text(
                    stringResource(R.string.pairing_with, ui.pairing),
                    style = MaterialTheme.typography.titleLarge,
                )
                val code = ui.pairingCode
                if (code == null) {
                    Text(stringResource(R.string.pairing_waiting))
                } else if (ui.pairingConfirm) {
                    Text(stringResource(R.string.pairing_confirm_if_showed))
                    Text(code, style = MaterialTheme.typography.displaySmall)
                    Text(stringResource(R.string.pairing_codes_differ))
                    Button(onClick = confirmPair) {
                        Text(stringResource(R.string.action_codes_match))
                    }
                } else {
                    Text(stringResource(R.string.pairing_allow_if_shows))
                    Text(code, style = MaterialTheme.typography.displaySmall)
                    Text(stringResource(R.string.pairing_codes_differ))
                }
                TextButton(onClick = cancelPair) { Text(stringResource(R.string.action_cancel)) }
            }
        }
    }
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(
            stringResource(R.string.nearby),
            style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.weight(1f),
        )
        TextButton(onClick = refresh, enabled = !busy) {
            Text(stringResource(R.string.action_refresh))
        }
    }
    val nearby = ui.nearby.filter { n -> ui.saved.none { it.fingerprint == n.fingerprint } }
    if (nearby.isEmpty() && ui.incompatible.isEmpty())
        Text(
            stringResource(if (ui.discovering) R.string.nearby_looking else R.string.nearby_none),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    nearby.forEach { computer ->
        ChoiceCard(computer.name, stringResource(R.string.nearby_tap_to_pair), enabled = !busy) {
            candidate = computer
        }
    }
    ui.incompatible.forEach { computer ->
        ChoiceCard(
            computer.name,
            stringResource(
                if (computer.newer) R.string.nearby_update_app else R.string.nearby_update_computer
            ),
            enabled = false,
        ) {}
    }
    OutlinedButton(
        onClick = { manualDialog = true },
        enabled = !busy,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text(stringResource(R.string.pair_by_ip))
    }
    candidate?.let { computer ->
        AlertDialog(
            containerColor = MaterialTheme.colorScheme.surface,
            onDismissRequest = { candidate = null },
            title = { Text(stringResource(R.string.pair_with_title, computer.name)) },
            text = { Text(stringResource(R.string.pair_with_body, computer.name)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        pair(computer)
                        candidate = null
                    },
                    enabled = !busy,
                ) {
                    Text(stringResource(R.string.action_pair))
                }
            },
            dismissButton = {
                TextButton(onClick = { candidate = null }) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
    if (manualDialog) ManualPairDialog(dismiss = { manualDialog = false }, pair = pair)
}

@Composable
private fun ManualPairDialog(dismiss: () -> Unit, pair: (NearbyComputer) -> Unit) {
    var address by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    val endpoint = manualEndpoint(address)
    val defaultName = stringResource(R.string.computer_default_name)
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.pair_by_ip)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(stringResource(R.string.pair_by_ip_body))
                OutlinedTextField(
                    value = address,
                    onValueChange = { address = it.take(21) },
                    label = { Text(stringResource(R.string.ip_address)) },
                    placeholder = { Text("192.168.1.10") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(64) },
                    label = { Text(stringResource(R.string.computer_name_optional)) },
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    pair(NearbyComputer(name.trim().ifEmpty { defaultName }, "", endpoint!!))
                    dismiss()
                },
                enabled = endpoint != null,
            ) {
                Text(stringResource(R.string.action_pair))
            }
        },
        dismissButton = {
            TextButton(onClick = dismiss) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

@Composable
private fun MicrophoneCard(
    settings: Settings,
    status: Snapshot,
    session: SessionStatus,
    busy: Boolean,
    start: (Settings) -> Unit,
) {
    val takeover = status.generation.takeIf { status.inUse }
    DirectionCard(
        stringResource(R.string.microphone),
        stringResource(R.string.microphone_description),
        settings.mic,
        status.error(0),
        busy,
        toggle = { on ->
            start(
                settings.copy(mic = on, muted = false, input = 0, output = 0, takeover = takeover)
            )
        },
    ) {
        SettingRow(
            stringResource(R.string.mute),
            if (settings.muted) stringResource(R.string.mute_description) else null,
            enabled = !busy && takeover == null,
            onClick = { start(settings.copy(muted = !settings.muted)) },
        ) {
            Switch(
                checked = settings.muted,
                onCheckedChange = null,
                enabled = !busy && takeover == null,
            )
        }
    }
}

@Composable
private fun ListenCard(
    settings: Settings,
    status: Snapshot,
    session: SessionStatus,
    busy: Boolean,
    start: (Settings) -> Unit,
    edit: (Settings) -> Unit,
    apply: () -> Unit,
    route: (ULong, Route) -> Unit,
    refreshRoutes: () -> Unit,
) {
    val takeover = status.generation.takeIf { status.inUse }
    val listeningVolume = stringResource(R.string.volume_listening)
    DirectionCard(
        stringResource(R.string.listen),
        stringResource(R.string.listen_description),
        settings.playback,
        status.error(1),
        busy,
        toggle = { on ->
            start(settings.copy(playback = on, input = 0, output = 0, takeover = takeover))
        },
    ) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
            Row(Modifier.fillMaxWidth()) {
                Text(
                    stringResource(R.string.volume),
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyLarge,
                )
                Text(
                    stringResource(R.string.volume_percent, (settings.playbackGain * 100).toInt()),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Slider(
                value = settings.playbackGain,
                onValueChange = { edit(settings.copy(playbackGain = it)) },
                valueRange = 0f..2f,
                enabled = !busy && !status.inUse,
                modifier = Modifier.semantics { contentDescription = listeningVolume },
                onValueChangeFinished = apply,
            )
            Text(
                stringResource(R.string.volume_boost_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        ComputerOutput(
            status.routes,
            enabled = !busy && status.connected && !status.inUse,
            route = route,
            refresh = refreshRoutes,
        )
    }
}

@Composable
private fun DirectionCard(
    title: String,
    description: String,
    on: Boolean,
    error: UiText?,
    busy: Boolean,
    toggle: (Boolean) -> Unit,
    details: @Composable ColumnScope.() -> Unit,
) {
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            Row(
                Modifier.fillMaxWidth()
                    .toggleable(
                        value = on,
                        enabled = !busy,
                        role = Role.Switch,
                        onValueChange = toggle,
                    )
                    .semantics { contentDescription = title }
                    .padding(horizontal = 20.dp, vertical = 14.dp),
                horizontalArrangement = Arrangement.spacedBy(16.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(title, style = MaterialTheme.typography.titleMedium)
                    Text(
                        description,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(checked = on, onCheckedChange = null, enabled = !busy)
            }
            if (on) {
                HorizontalDivider(Modifier.padding(horizontal = 20.dp))
                details()
            }
            error?.let {
                Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                    Notice(it.resolve(), error = true)
                }
            }
        }
    }
}

@Composable
private fun ComputerOutput(
    routes: DesktopRoutes?,
    enabled: Boolean,
    route: (ULong, Route) -> Unit,
    refresh: () -> Unit,
) {
    var outputs by remember { mutableStateOf(false) }
    SettingRow(
        stringResource(R.string.sound_from),
        routes?.let { routeDescription(it.effective, it) } ?: stringResource(R.string.unavailable),
        enabled = enabled && routes != null,
        onClick = {
            refresh()
            outputs = true
        },
    )
    if (outputs && routes != null) {
        val mirror = routes.effective as? Route.Mirror
        fun choose(name: String?) {
            route(routes.revision, Route.Mirror(name))
            outputs = false
        }
        AudioChoiceSheet(stringResource(R.string.sound_from), dismiss = { outputs = false }) {
            AudioChoiceRow(
                stringResource(R.string.output_default),
                selected = mirror != null && mirror.name == null,
                enabled = enabled,
                description = stringResource(R.string.output_default_description),
            ) {
                choose(null)
            }
            routes.endpoints.forEach { endpoint ->
                AudioChoiceRow(
                    endpoint.label,
                    selected = mirror?.name == endpoint.name,
                    enabled = enabled,
                ) {
                    choose(endpoint.name)
                }
            }
        }
    }
}

@Composable
private fun SettingsScreen(
    ui: UiState,
    status: Snapshot,
    busy: Boolean,
    edit: (Settings) -> Unit,
    apply: () -> Unit,
    voice: (ULong, VoiceSettings) -> Unit,
) {
    var sources by remember { mutableStateOf(false) }
    val context = LocalContext.current
    SectionHeader(stringResource(R.string.settings_phone_microphone))
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            SettingRow(
                stringResource(R.string.recording_mode),
                recordingModeDescription(context, ui.settings.source, status),
                enabled = !busy,
                onClick = { sources = true },
            )
        }
    }
    if (sources)
        AudioChoiceSheet(stringResource(R.string.recording_mode), dismiss = { sources = false }) {
            microphoneSources.forEach { source ->
                AudioChoiceRow(
                    stringResource(source.title),
                    selected = ui.settings.source == source.value,
                    enabled = !busy,
                    description = stringResource(source.description),
                ) {
                    edit(ui.settings.copy(source = source.value))
                    apply()
                    sources = false
                }
            }
        }
    SectionHeader(stringResource(R.string.voice_header))
    voiceRuntimeDescription(status)?.let { Notice(it.resolve()) }
    val routes = status.routes
    if (routes == null)
        OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
            Column(Modifier.padding(vertical = 6.dp)) {
                SettingRow(
                    stringResource(R.string.voice_not_connected),
                    stringResource(R.string.voice_connect_hint),
                    enabled = false,
                )
            }
        }
    else
        VoiceControls(
            ui.selected,
            routes.revision,
            VoicePreferences.from(routes.voice),
            enabled = !busy && !status.inUse && status.connected,
            supported = routes.suppressionAlgorithms,
            failure = ui.session.error,
            inputRms = status.microphoneRms,
            apply = voice,
        )
    SectionHeader(stringResource(R.string.about))
    val uriHandler = LocalUriHandler.current
    var details by rememberSaveable { mutableStateOf(false) }
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            SettingRow(
                stringResource(R.string.about_version, BuildConfig.VERSION_NAME),
                stringResource(R.string.about_license),
            )
            SettingRow(
                stringResource(R.string.source_code),
                "github.com/ArturoAtomplay/litz-audio",
                onClick = { uriHandler.openUri(SOURCE_URL) },
            )
            StoreOptions()
            SettingRow(
                stringResource(R.string.diagnostics),
                stringResource(R.string.diagnostics_description),
                onClick = { details = !details },
            ) {
                Text(
                    stringResource(if (details) R.string.action_hide else R.string.action_show),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            if (details) {
                val sections = diagnostics(context, ui, status)
                val clipLabel = stringResource(R.string.diagnostics)
                Surface(
                    Modifier.fillMaxWidth().padding(horizontal = 16.dp),
                    shape = RoundedCornerShape(16.dp),
                    color = MaterialTheme.colorScheme.surfaceContainerHighest,
                ) {
                    Column(
                        Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        sections.forEach { section ->
                            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                Text(
                                    section.title,
                                    style = MaterialTheme.typography.labelLarge,
                                    color = MaterialTheme.colorScheme.primary,
                                )
                                section.rows.forEach { (label, value) ->
                                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                        Text(
                                            label,
                                            Modifier.weight(1f),
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                        Text(
                                            value,
                                            Modifier.weight(1.4f),
                                            style = MaterialTheme.typography.bodySmall,
                                            textAlign = TextAlign.End,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
                TextButton(
                    onClick = {
                        context
                            .getSystemService(ClipboardManager::class.java)
                            .setPrimaryClip(
                                ClipData.newPlainText(
                                    clipLabel,
                                    diagnosticText(diagnostics(context.inEnglish(), ui, status)),
                                )
                            )
                    },
                    modifier = Modifier.padding(horizontal = 8.dp),
                ) {
                    Text(stringResource(R.string.diagnostics_copy))
                }
            }
        }
    }
}

@Composable
private fun ChoiceCard(name: String, state: String, enabled: Boolean, action: () -> Unit) {
    OutlinedCard(
        onClick = action,
        enabled = enabled,
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(24.dp),
    ) {
        SettingValue(name, state, enabled = enabled)
    }
}

@Composable
private fun Notice(text: String, error: Boolean = false) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        color =
            if (error) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.surfaceVariant,
        contentColor =
            if (error) MaterialTheme.colorScheme.onErrorContainer
            else MaterialTheme.colorScheme.onSurfaceVariant,
        shape = RoundedCornerShape(20.dp),
    ) {
        Text(text, Modifier.padding(16.dp))
    }
}
