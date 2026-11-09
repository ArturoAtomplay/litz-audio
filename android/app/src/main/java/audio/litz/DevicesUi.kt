package audio.litz

import android.content.ClipData
import android.content.ClipboardManager
import androidx.activity.compose.BackHandler
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
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import org.json.JSONObject

internal val microphoneSources =
    listOf(
        MicrophoneSource(0, "Automatic", "Recommended for most phones"),
        MicrophoneSource(1, "Standard microphone", "General recording mode"),
        MicrophoneSource(7, "Calls and meetings", "Optimized for voice calls"),
        MicrophoneSource(6, "Speech recognition", "Prioritizes clear speech"),
        MicrophoneSource(10, "Live performance", "Low latency when supported"),
        MicrophoneSource(5, "Camera microphone", "Camera recording mode"),
        MicrophoneSource(9, "Raw microphone", "Less processing when supported"),
    )

internal data class MicrophoneSource(val value: Int, val title: String, val description: String)

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
    disconnect: () -> Unit,
    pair: (NearbyComputer) -> Unit,
    cancelPair: () -> Unit,
    select: (SavedComputer) -> Unit,
    refresh: () -> Unit,
    edit: (Settings) -> Unit,
    apply: () -> Unit,
    forget: () -> Unit,
    route: (Long, Any) -> Unit = { _, _ -> },
    refreshRoutes: () -> Unit = {},
    voice: (Long, org.json.JSONObject) -> Unit = { _, _ -> },
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
                        when (screen) {
                            Screen.Home -> "Litz"
                            Screen.Computers -> "Computers"
                            Screen.Settings -> "Settings"
                        },
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.headlineSmall,
                    )
                    if (screen == Screen.Home)
                        TextButton(onClick = { screen = Screen.Settings }) { Text("Settings") }
                    else TextButton(onClick = { screen = Screen.Home }) { Text("Done") }
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
                ui.message?.takeUnless { it == ui.session.error }?.let { Notice(it) }
                error?.let { Notice(it, error = true) }
                when (screen) {
                    Screen.Home ->
                        if (selected == null) {
                            Text("Add your computer", style = MaterialTheme.typography.titleLarge)
                            Text(
                                "Install Litz on your computer and connect both devices to the same network. Then pick your computer below.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            AddComputer(ui, busy, pair, cancelPair, refresh)
                        } else {
                            ComputerCard(
                                selected,
                                ui,
                                status,
                                busy,
                                connect = connect,
                                disconnect = disconnect,
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
                            Text("Your computers", style = MaterialTheme.typography.titleLarge)
                            selected?.let {
                                OutlinedCard(
                                    Modifier.fillMaxWidth(),
                                    shape = RoundedCornerShape(24.dp),
                                ) {
                                    SettingValue(it.name, "Using now", enabled = false)
                                }
                            }
                            others.forEach { computer ->
                                ChoiceCard(computer.name, "Tap to switch", enabled = !busy) {
                                    select(computer)
                                }
                            }
                        }
                        AddComputer(ui, busy, pair, cancelPair, refresh)
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
    disconnect: () -> Unit,
    change: () -> Unit,
    forget: () -> Unit,
    pairAgain: (NearbyComputer) -> Unit,
) {
    val nearby = ui.nearby.firstOrNull { it.fingerprint == computer.fingerprint }
    val connecting =
        status.state == "Reconnecting" || (ui.session.pending && !ui.quiet && !status.connected)
    val (line, color) =
        when {
            !status.reconnectAllowed && !status.connected ->
                "This computer no longer trusts this phone" to MaterialTheme.colorScheme.error
            status.inUse -> "In use by ${status.owner}" to MaterialTheme.colorScheme.tertiary
            status.connected -> "Connected" to MaterialTheme.colorScheme.primary
            connecting -> "Connecting…" to MaterialTheme.colorScheme.tertiary
            ui.disconnected -> "Disconnected" to MaterialTheme.colorScheme.outline
            nearby != null && ui.session.error != null ->
                "Found on your network, but not answering" to MaterialTheme.colorScheme.outline
            else -> "Offline" to MaterialTheme.colorScheme.outline
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
                        Icon(painterResource(R.drawable.ic_more), contentDescription = "More")
                    }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(
                            text = { Text(if (status.connected) "Disconnect" else "Connect") },
                            enabled = !busy,
                            onClick = {
                                menu = false
                                if (status.connected) disconnect() else connect()
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("Change computer") },
                            enabled = !busy,
                            onClick = {
                                menu = false
                                change()
                            },
                        )
                        DropdownMenuItem(
                            text = {
                                Text(
                                    "Forget this computer",
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
                        !status.reconnectAllowed -> "Pair again to use it with this phone."
                        ui.disconnected -> "Connect when you want to use audio."
                        nearby != null ->
                            "Make sure your computer's firewall allows Litz on UDP port 47165."
                        else -> "Litz connects on its own when ${computer.name} is on."
                    },
                    Modifier.padding(top = 8.dp, end = 12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSecondaryContainer,
                )
                Row(Modifier.padding(top = 12.dp)) {
                    if (!status.reconnectAllowed)
                        nearby?.let {
                            Button(onClick = { pairAgain(it) }, enabled = !busy) {
                                Text("Pair again")
                            }
                        }
                    else
                        Button(onClick = connect, enabled = !busy) {
                            Text(if (ui.disconnected) "Connect" else "Try again")
                        }
                }
            }
        }
    }
    if (confirmForget)
        AlertDialog(
            containerColor = MaterialTheme.colorScheme.surface,
            onDismissRequest = { confirmForget = false },
            title = { Text("Forget ${computer.name}?") },
            text = { Text("To use it again, pair it from the Computers screen.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmForget = false
                        forget()
                    }
                ) {
                    Text("Forget", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = { TextButton(onClick = { confirmForget = false }) { Text("Cancel") } },
        )
}

@Composable
private fun AddComputer(
    ui: UiState,
    busy: Boolean,
    pair: (NearbyComputer) -> Unit,
    cancelPair: () -> Unit,
    refresh: () -> Unit,
) {
    var candidate by remember { mutableStateOf<NearbyComputer?>(null) }
    var manualDialog by remember { mutableStateOf(false) }
    if (ui.pairing != null) {
        Card(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
            Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                LitzBars(Modifier.size(width = 54.dp, height = 40.dp))
                Text("Pairing with ${ui.pairing}", style = MaterialTheme.typography.titleLarge)
                val code = ui.pairingCode
                if (code == null) {
                    Text("Waiting for your computer…")
                } else {
                    Text("Allow the connection on your computer if it shows:")
                    Text(code, style = MaterialTheme.typography.displaySmall)
                    Text("If the codes differ, cancel.")
                }
                TextButton(onClick = cancelPair) { Text("Cancel") }
            }
        }
    }
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text("Nearby", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
        TextButton(onClick = refresh, enabled = !busy) { Text("Refresh") }
    }
    val nearby = ui.nearby.filter { n -> ui.saved.none { it.fingerprint == n.fingerprint } }
    if (nearby.isEmpty())
        Text(
            if (ui.discovering) "Looking for computers running Litz…"
            else "No computers found. Tap Refresh to look again.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    nearby.forEach { computer ->
        ChoiceCard(computer.name, "Tap to pair", enabled = !busy) { candidate = computer }
    }
    OutlinedButton(
        onClick = { manualDialog = true },
        enabled = !busy,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Text("Pair by IP address")
    }
    candidate?.let { computer ->
        AlertDialog(
            containerColor = MaterialTheme.colorScheme.surface,
            onDismissRequest = { candidate = null },
            title = { Text("Pair with ${computer.name}?") },
            text = {
                Text(
                    "Tap Pair, then allow the connection on ${computer.name}. You only do this once."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        pair(computer)
                        candidate = null
                    },
                    enabled = !busy,
                ) {
                    Text("Pair")
                }
            },
            dismissButton = { TextButton(onClick = { candidate = null }) { Text("Cancel") } },
        )
    }
    if (manualDialog) ManualPairDialog(dismiss = { manualDialog = false }, pair = pair)
}

@Composable
private fun ManualPairDialog(dismiss: () -> Unit, pair: (NearbyComputer) -> Unit) {
    var address by remember { mutableStateOf("") }
    var name by remember { mutableStateOf("") }
    val endpoint = manualEndpoint(address)
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Pair by IP address") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(
                    "Enter the computer's local IPv4 address. Litz verifies the computer's identity before saving it."
                )
                OutlinedTextField(
                    value = address,
                    onValueChange = { address = it.take(21) },
                    label = { Text("IP address") },
                    placeholder = { Text("192.168.1.10") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(64) },
                    label = { Text("Computer name (optional)") },
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(
                onClick = {
                    pair(NearbyComputer(name.trim().ifEmpty { "Computer" }, "", endpoint!!))
                    dismiss()
                },
                enabled = endpoint != null,
            ) {
                Text("Pair")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Cancel") } },
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
        "Microphone",
        "Use your phone as a microphone",
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
            "Mute",
            if (settings.muted) "Your computer hears silence" else null,
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
    route: (Long, Any) -> Unit,
    refreshRoutes: () -> Unit,
) {
    val takeover = status.generation.takeIf { status.inUse }
    DirectionCard(
        "Listen",
        "Hear your computer on your phone",
        settings.playback,
        status.error(1),
        busy,
        toggle = { on ->
            start(settings.copy(playback = on, input = 0, output = 0, takeover = takeover))
        },
    ) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
            Row(Modifier.fillMaxWidth()) {
                Text("Volume", Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                Text(
                    "${(settings.playbackGain * 100).toInt()}%",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Slider(
                value = settings.playbackGain,
                onValueChange = { edit(settings.copy(playbackGain = it)) },
                valueRange = 0f..2f,
                enabled = !busy && !status.inUse,
                modifier = Modifier.semantics { contentDescription = "Listening volume" },
                onValueChangeFinished = apply,
            )
            Text(
                "Above 100% boosts quiet audio.",
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
    error: String?,
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
                    Notice(it, error = true)
                }
            }
        }
    }
}

@Composable
private fun ComputerOutput(
    routes: JSONObject?,
    enabled: Boolean,
    route: (Long, Any) -> Unit,
    refresh: () -> Unit,
) {
    var outputs by remember { mutableStateOf(false) }
    SettingRow(
        "Sound from",
        routes?.let { routeDescription(it.opt("effective"), it) } ?: "Unavailable",
        enabled = enabled && routes != null,
        onClick = {
            refresh()
            outputs = true
        },
    )
    if (outputs && routes != null) {
        val mirror = routes.optJSONObject("effective")?.optJSONObject("Mirror")
        fun choose(name: String?) {
            route(
                routes.getLong("revision"),
                JSONObject().put("Mirror", JSONObject().put("name", name ?: JSONObject.NULL)),
            )
            outputs = false
        }
        AudioChoiceSheet("Sound from", dismiss = { outputs = false }) {
            AudioChoiceRow(
                "Default output",
                selected = mirror?.isNull("name") == true,
                enabled = enabled,
                description = "Follows your computer when it changes output",
            ) {
                choose(null)
            }
            routes.optJSONArray("endpoints")?.let { endpoints ->
                for (i in 0 until endpoints.length()) {
                    val device = endpoints.getJSONObject(i)
                    val name = device.getString("name")
                    AudioChoiceRow(
                        device.getString("label"),
                        selected = mirror?.optString("name") == name,
                        enabled = enabled,
                    ) {
                        choose(name)
                    }
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
    voice: (Long, JSONObject) -> Unit,
) {
    var sources by remember { mutableStateOf(false) }
    SectionHeader("Phone microphone")
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            SettingRow(
                "Recording mode",
                recordingModeDescription(ui.settings.source, status),
                enabled = !busy,
                onClick = { sources = true },
            )
        }
    }
    if (sources)
        AudioChoiceSheet("Recording mode", dismiss = { sources = false }) {
            microphoneSources.forEach { source ->
                AudioChoiceRow(
                    source.title,
                    selected = ui.settings.source == source.value,
                    enabled = !busy,
                    description = source.description,
                ) {
                    edit(ui.settings.copy(source = source.value))
                    apply()
                    sources = false
                }
            }
        }
    SectionHeader("Voice processing on your computer")
    voiceRuntimeDescription(status)?.let { Notice(it) }
    val saved = status.routes?.optJSONObject("voice")
    if (saved == null)
        OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
            Column(Modifier.padding(vertical = 6.dp)) {
                SettingRow(
                    "Not connected",
                    "Connect to your computer to adjust voice processing.",
                    enabled = false,
                )
            }
        }
    else
        VoiceControls(
            ui.selected,
            status.routes.getLong("revision"),
            VoicePreferences.from(saved),
            enabled = !busy && !status.inUse && status.connected,
            supported =
                remember(status.routes.optJSONArray("suppression_algorithms")?.toString()) {
                    status.routes.optJSONArray("suppression_algorithms")?.let { values ->
                        (0 until values.length()).map { values.getString(it) }
                    } ?: emptyList()
                },
            failure = ui.session.error,
            inputRms = status.microphoneRms,
            apply = voice,
        )
    SectionHeader("About")
    val uriHandler = LocalUriHandler.current
    val context = LocalContext.current
    var details by rememberSaveable { mutableStateOf(false) }
    OutlinedCard(Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
        Column(Modifier.padding(vertical = 6.dp)) {
            SettingRow("Litz ${BuildConfig.VERSION_NAME}", "Free software, GNU GPL 3.0 or later")
            SettingRow(
                "Source code",
                "github.com/ArturoAtomplay/litz-audio",
                onClick = { uriHandler.openUri(SOURCE_URL) },
            )
            StoreOptions()
            SettingRow(
                "Diagnostics",
                "Details to include in a bug report",
                onClick = { details = !details },
            ) {
                Text(
                    if (details) "Hide" else "Show",
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            if (details) {
                val sections = diagnostics(ui, status)
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
                                ClipData.newPlainText("Litz diagnostics", diagnosticText(sections))
                            )
                    },
                    modifier = Modifier.padding(horizontal = 8.dp),
                ) {
                    Text("Copy report")
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
