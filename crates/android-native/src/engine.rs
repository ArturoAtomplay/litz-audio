fn default_source() -> i32 {
    0
}
use crate::Identity;
use crate::audio::{Audio, GainLimiter};
use litz_core::media::codec::Encoder;
use litz_core::media::receiver::Receiver;
use litz_core::protocol::{
    Code, Config, DesktopRoutes, FRAME, MAX_DATAGRAM, Message, Record, Route, Stream,
};
use litz_core::transport::{self, Control, MediaSender, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub mic: bool,
    pub playback: bool,
    pub muted: bool,
    pub mic_gain: f32,
    pub playback_gain: f32,
    pub input: i32,
    #[serde(default = "default_source")]
    pub source: i32,
    pub output: i32,
    pub reconnect: bool,
    pub takeover: Option<u64>,
    pub raw_supported: bool,
    pub headset_ids: Vec<i32>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mic: false,
            playback: false,
            muted: false,
            mic_gain: 1.0,
            playback_gain: 1.0,
            input: 0,
            source: 0,
            output: 0,
            reconnect: true,
            takeover: None,
            raw_supported: false,
            headset_ids: Vec::new(),
        }
    }
}
impl Settings {
    fn validate(&self) -> Result<()> {
        if !self.mic_gain.is_finite()
            || !self.playback_gain.is_finite()
            || !(0.0..=2.0).contains(&self.mic_gain)
            || !(0.0..=2.0).contains(&self.playback_gain)
            || ![0, 1, 5, 6, 7, 9, 10].contains(&self.source)
            || self.headset_ids.len() > 64
            || self.input < 0
            || self.output < 0
        {
            return Err("invalid gain/device".into());
        }
        Ok(())
    }
}
fn capture_source(settings: &Settings) -> i32 {
    if settings.source == 0 {
        if settings.raw_supported { 9 } else { 6 }
    } else {
        settings.source
    }
}
fn capture_sources(settings: &Settings) -> Vec<i32> {
    let source = capture_source(settings);
    let mut sources = vec![source];
    if settings.source == 0 {
        if source == 9 {
            sources.push(6);
        }
        sources.push(1);
    }
    sources.dedup();
    sources
}

fn stream_restarts(old: &Settings, desired: &Settings, missing_codec: [bool; 2]) -> [bool; 2] {
    let source_changed = old.mic && capture_source(old) != capture_source(desired);
    [
        desired.mic
            && !desired.muted
            && (old.mic != desired.mic
                || old.muted != desired.muted
                || old.input != desired.input
                || source_changed
                || missing_codec[0]),
        desired.playback
            && (old.playback != desired.playback
                || old.output != desired.output
                || missing_codec[1]),
    ]
}
fn can_reuse_audio(
    old: &Settings,
    desired: &Settings,
    encoder_present: bool,
    receiver_present: bool,
) -> bool {
    old.mic == desired.mic
        && old.playback == desired.playback
        && (!old.mic || capture_source(old) == capture_source(desired))
        && old.input == desired.input
        && old.output == desired.output
        && (!desired.mic || desired.muted || encoder_present || old.muted)
        && (!desired.playback || receiver_present)
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Status,
    Connect,
    RefreshRoutes,
    Disconnect,
    Forget,
    Address {
        endpoint: std::net::SocketAddr,
    },
    Route {
        expected_revision: u64,
        route: Route,
    },
    Voice {
        expected_revision: u64,
        voice: litz_core::protocol::VoiceSettings,
    },
    Set {
        settings: Settings,
    },
    Stop,
}
struct Request {
    command: Command,
    reply: mpsc::SyncSender<String>,
}
pub struct Engine {
    tx: Option<tokio::sync::mpsc::Sender<Request>>,
    worker: Option<std::thread::JoinHandle<()>>,
    cancel: Arc<tokio::sync::Notify>,
    cancelled: Arc<AtomicBool>,
}
impl Engine {
    pub fn try_new(identity: Identity) -> Result<Self> {
        #[cfg(target_os = "android")]
        {
            static PANIC_LOG: std::sync::Once = std::sync::Once::new();
            PANIC_LOG.call_once(|| {
                let previous = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    unsafe extern "C" {
                        fn __android_log_write(
                            priority: libc::c_int,
                            tag: *const libc::c_char,
                            text: *const libc::c_char,
                        ) -> libc::c_int;
                    }
                    if let Ok(message) = std::ffi::CString::new(info.to_string()) {
                        // SAFETY: both C strings stay live for the synchronous log call.
                        unsafe { __android_log_write(6, c"LitzNative".as_ptr(), message.as_ptr()) };
                    }
                    previous(info);
                }));
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let cancel = Arc::new(tokio::sync::Notify::new());
        let stopped = cancel.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let worker = std::thread::Builder::new()
            .name("litz-engine".into())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                #[cfg(target_os = "android")]
                // SAFETY: adjusts only the calling thread; failure keeps the default.
                unsafe {
                    libc::setpriority(libc::PRIO_PROCESS as _, libc::gettid() as _, -16);
                }
                runtime.block_on(Box::pin(run(identity, rx, stopped, worker_cancelled)));
            })?;
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
            cancel,
            cancelled,
        })
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.notify_one();
    }
    pub fn command(&self, json: &str) -> Result<String> {
        let command = serde_json::from_str(json).map_err(|_| "invalid native command")?;
        let (reply, result) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .ok_or("engine closed")?
            .blocking_send(Request { command, reply })
            .map_err(|_| "engine closed")?;
        Ok(result.recv_timeout(Duration::from_secs(30))?)
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.cancel();
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Media {
    connection: quinn::Connection,
    audio: Option<Audio>,
    listening: Option<Config>,
    encoder: Option<Encoder>,
    sender: MediaSender,
    settings: Settings,
    reduced: Instant,
    captured: [f32; FRAME],
    microphone_rms: f32,
    last_capture_at: Instant,
    captured_len: usize,
    capture_block: Option<crate::audio::Captured>,
    capture_offset: usize,
    pending_send: Option<usize>,
    microphone_level: GainLimiter,
    packet: [u8; MAX_DATAGRAM],
    dropped: u64,
}
impl Media {
    fn clear(&mut self) {
        self.audio.take();
        self.listening = None;
        self.encoder = None;
        self.captured_len = 0;
        self.microphone_rms = 0.0;
        self.last_capture_at = Instant::now();
        self.capture_block = None;
        self.capture_offset = 0;
        self.pending_send = None;
        self.sender = MediaSender::default();
    }
    fn listen(&mut self, config: Option<Config>) -> Result<()> {
        self.listening = config;
        if let Some(audio) = &mut self.audio {
            let receiver = config.map(Receiver::try_new).transpose()?.map(Box::new);
            audio.listen(receiver)?;
        }
        Ok(())
    }
    async fn event(&mut self) -> Result<()> {
        tokio::select! {
            packet = self.connection.read_datagram() => {
                let packet = packet?;
                if self.listening.is_some() && let Some(audio) = &mut self.audio {
                    audio.push_packet(&packet);
                }
                self.handoffs().await?;
            }
            ready = async { self.audio.as_ref().unwrap().wake.wait().await }, if self.audio.is_some() => {
                ready?;
                self.handoffs().await?;
            }
        }
        Ok(())
    }
    async fn handoffs(&mut self) -> Result<()> {
        let Some(audio) = &mut self.audio else {
            return Ok(());
        };
        if audio.fault() {
            audio.silence();
            if audio.info().error != 0 {
                return Err("Audio device disconnected".into());
            }
            return Err(Failure::AudioDiscontinuity.into());
        }
        audio.collect();
        audio.set_playback_gain(self.settings.playback_gain);
        if self.pending_send.is_some() && self.last_capture_at.elapsed() > Duration::from_millis(40)
        {
            self.pending_send = None;
        }
        for _ in 0..32 {
            // event() is cancelled whenever a control/status request wins select!.
            // Keep both the encoded packet and the unread callback samples in
            // Media until the transport has actually accepted them.
            if let Some(len) = self.pending_send {
                if !self
                    .sender
                    .send(&self.connection, &self.packet[..len])
                    .await?
                {
                    self.dropped += 1;
                }
                self.pending_send = None;
            }
            if self.capture_block.is_none() {
                self.capture_block = audio.capture();
                self.capture_offset = 0;
            }
            let Some(block) = &self.capture_block else {
                break;
            };
            if block.at.elapsed() > Duration::from_millis(40) {
                self.capture_block = None;
                self.captured_len = 0;
                self.pending_send = None;
                continue;
            }
            let Some(e) = &mut self.encoder else {
                self.capture_block = None;
                continue;
            };
            while self.capture_offset < block.len {
                let s = block.samples[self.capture_offset];
                self.capture_offset += 1;
                self.captured[self.captured_len] = gain(s, 1.0);
                self.captured_len += 1;
                if self.captured_len < FRAME {
                    continue;
                }
                if audio.fault() {
                    audio.silence();
                    return Err(Failure::AudioDiscontinuity.into());
                }
                self.microphone_level
                    .process::<1>(&mut self.captured, self.settings.mic_gain);
                self.microphone_rms =
                    (self.captured.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt();
                self.last_capture_at = Instant::now();
                let len = e.encode(
                    &self.captured,
                    &mut self.packet,
                    self.connection.max_datagram_size().unwrap_or(0),
                )?;
                if audio.fault() {
                    audio.silence();
                    return Err(Failure::AudioDiscontinuity.into());
                }
                self.captured_len = 0;
                self.pending_send = Some(len);
                if !self
                    .sender
                    .send(&self.connection, &self.packet[..len])
                    .await?
                {
                    self.dropped += 1;
                    if self.reduced.elapsed() > Duration::from_secs(1) {
                        e.reduce_bitrate()?;
                        self.reduced = Instant::now();
                    }
                } else if self.reduced.elapsed() > Duration::from_secs(10) {
                    e.recover_bitrate()?;
                    self.reduced = Instant::now();
                }
                self.pending_send = None;
            }
            self.capture_block = None;
        }
        Ok(())
    }
}
fn gain(sample: f32, gain: f32) -> f32 {
    if sample.is_finite() {
        (sample * gain).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}
struct Connected {
    _endpoint: quinn::Endpoint,
    control: Control,
    media: Box<Media>,
    request: u32,
    epoch: u32,
    revision: u64,
    running: [Option<Config>; 2],
    direction_errors: [Option<String>; 2],
    desktop: Option<DesktopRoutes>,
    owner: Option<String>,
    generation: u64,
    owned: bool,
}
type Handoff = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<Connected>>>>>;
impl Drop for Connected {
    fn drop(&mut self) {
        self.media.clear();
        self.media.connection.close(0u8.into(), b"phone stopped");
    }
}
impl Connected {
    async fn event(&mut self) -> Result<()> {
        let capture_deadline =
            tokio::time::Instant::from_std(self.media.last_capture_at + Duration::from_secs(3));
        let watching_capture = self.media.encoder.is_some() && self.media.audio.is_some();
        tokio::select! {
            result = self.media.event() => result,
            _ = tokio::time::sleep_until(capture_deadline), if watching_capture => {
                Err(Failure::AudioDiscontinuity.into())
            }
            record = self.control.read() => {
                match record {
                    Ok(Record { request_id: 0, message: Message::Capabilities { desktop: Some(desktop), .. }, .. }) => {
                        self.revision = desktop.revision;
                        self.desktop = Some(desktop);
                        Ok(())
                    }
                    Ok(Record { request_id: 0, message: Message::Error { code: Code::RouteConflict }, .. }) => {
                        let mut remaining = self.media.settings.clone();
                        remaining.playback = false;
                        self.apply(&remaining).await?;
                        self.direction_errors[1] = Some("Desktop endpoint disappeared or route conflicted. Choose a computer output in Settings, then retry Listen.".into());
                        self.refresh_routes().await?;
                        Ok(())
                    }
                    Ok(_) => Err("desktop route/state changed".into()),
                    Err(e) => Err(e),
                }
            }
        }
    }
    async fn rpc(&mut self, message: Message) -> Result<Message> {
        self.request = self.request.wrapping_add(1).max(1);
        self.control
            .write(&Record::new(self.request, message))
            .await?;
        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return Err(Failure::ControlTimeout.into()),
                record = self.control.read() => {
                    let record = record?;
                    if record.request_id == 0 {
                        if let Message::Capabilities { desktop: Some(desktop), .. } = record.message {
                            self.revision = desktop.revision;
                            self.desktop = Some(desktop);
                            continue;
                        }
                        return Err("desktop route became unavailable".into());
                    }
                    if record.request_id != self.request { return Err("unexpected control response".into()); }
                    if let Message::Status { revision, running } = &record.message {
                        self.revision = *revision; self.running = *running;
                    }
                    if let Message::Ownership { generation, owner, owned } = &record.message {
                        self.generation = *generation; self.owner = owner.clone(); self.owned = *owned;
                    }
                    return Ok(record.message);
                }
                result = self.media.event() => { result?; }
            }
        }
    }
    async fn refresh_routes(&mut self) -> Result<()> {
        match self.rpc(Message::Capabilities { desktop: None }).await? {
            Message::Capabilities { desktop } => {
                if let Some(d) = &desktop {
                    self.revision = d.revision;
                }
                self.desktop = desktop;
                Ok(())
            }
            _ => Err("invalid Capabilities response".into()),
        }
    }
    async fn route(
        &mut self,
        expected_revision: u64,
        route: Route,
        desired: &Settings,
    ) -> Result<Option<String>> {
        self.acquire(None).await?;
        self.refresh_routes().await?;
        if expected_revision != self.revision {
            return Ok(Some(
                "Computer outputs changed. Refresh the output list and try again.".into(),
            ));
        }
        if self.running[1].is_some() {
            let _ = self
                .rpc(Message::Stop {
                    stream: Stream::Playback,
                })
                .await?;
        }
        self.media.listen(None)?;
        let response = self
            .rpc(Message::ApplySettings {
                expected_revision,
                route,
            })
            .await?;
        self.refresh_routes().await?;
        let old = &self.media.settings;
        let can_preserve_audio = self.media.audio.is_some()
            && old.mic == desired.mic
            && old.muted == desired.muted
            && old.source == desired.source
            && old.input == desired.input
            && old.output == desired.output;
        if can_preserve_audio {
            self.media.settings = desired.clone();
            if desired.playback {
                self.epoch = self.epoch.checked_add(1).ok_or("epoch exhausted")?;
                let config = Config {
                    stream: Stream::Playback,
                    epoch: self.epoch,
                };
                match self.rpc(Message::Start(config)).await? {
                    Message::Status { running, .. } if running[1] == Some(config) => {
                        self.media.listen(Some(config))?;
                        self.media.settings.playback = true;
                        self.direction_errors[1] = None;
                    }
                    Message::Error { code } => {
                        self.media.settings.playback = false;
                        self.direction_errors[1] = Some(format!("desktop: {code:?}"));
                    }
                    _ => return Err("Desktop did not restart listening after route change".into()),
                }
            } else {
                self.media.settings.playback = false;
            }
        } else {
            self.media.settings.playback = false;
            self.apply(desired).await?;
        }
        Ok(match response {
            Message::Status { .. } => None,
            Message::Error { code } => Some(format!(
                "Desktop source change refused or incomplete: {code:?}. Review the actual applied route and try again."
            )),
            _ => Some("Desktop did not acknowledge the route".into()),
        })
    }
    async fn voice(
        &mut self,
        expected_revision: u64,
        voice: litz_core::protocol::VoiceSettings,
    ) -> Result<Option<String>> {
        self.acquire(None).await?;
        let response = self
            .rpc(Message::ApplyVoice {
                expected_revision,
                voice,
            })
            .await?;
        self.refresh_routes().await?;
        if !self.media.settings.playback && !self.media.settings.mic {
            self.release().await?;
        }
        Ok(match response {
            Message::Status { .. } => None,
            Message::Error { code } => Some(format!(
                "Voice settings were not applied: {code:?}. Refresh and try again."
            )),
            _ => Some("Computer did not acknowledge voice settings".into()),
        })
    }
    async fn stop_streams(&mut self) -> Result<()> {
        self.media.clear(); // immediate local mute before any reliable RPC
        for stream in Stream::ALL {
            if self.running[stream.index()].is_some()
                && !matches!(
                    self.rpc(Message::Stop { stream }).await?,
                    Message::Status { .. }
                )
            {
                return Err("desktop stop refused".into());
            }
        }
        self.media.settings.mic = false;
        self.media.settings.playback = false;
        Ok(())
    }
    async fn release(&mut self) -> Result<()> {
        self.stop_streams().await?;
        if self.owned {
            self.rpc(Message::Release {}).await?;
        }
        Ok(())
    }
    async fn acquire(&mut self, expected: Option<u64>) -> Result<()> {
        if !self.owned {
            self.rpc(Message::Acquire {
                expected_generation: expected,
            })
            .await?;
            if !self.owned && expected.is_some_and(|g| g != self.generation) {
                self.rpc(Message::Acquire {
                    expected_generation: Some(self.generation),
                })
                .await?;
            }
            if !self.owned {
                return Err(format!(
                    "In use by {}. Turn on the microphone or listening here to move audio to this phone.",
                    self.owner.as_deref().unwrap_or("another phone")
                )
                .into());
            }
            self.refresh_routes().await?;
        }
        Ok(())
    }
    async fn apply(&mut self, desired: &Settings) -> Result<()> {
        if desired.mic || desired.playback {
            self.acquire(desired.takeover).await?;
        } else {
            return self.release().await;
        }
        let old = &self.media.settings;
        if self
            .media
            .audio
            .as_ref()
            .is_some_and(|audio| !audio.fault())
            && can_reuse_audio(
                old,
                desired,
                self.media.encoder.is_some(),
                self.media.listening.is_some(),
            )
        {
            if old.muted != desired.muted && desired.mic {
                if desired.muted {
                    self.media.encoder = None;
                    self.media.captured_len = 0;
                    self.rpc(Message::Stop {
                        stream: Stream::Mic,
                    })
                    .await?;
                } else {
                    self.epoch = self.epoch.checked_add(1).ok_or("epoch exhausted")?;
                    let config = Config {
                        stream: Stream::Mic,
                        epoch: self.epoch,
                    };
                    if !matches!(self.rpc(Message::Start(config)).await?, Message::Status { running, .. } if running[0] == Some(config))
                    {
                        return Err("Microphone could not resume".into());
                    }
                    self.media.captured_len = 0;
                    self.media.encoder = Some(Encoder::try_new(config)?);
                }
            }
            self.media.settings = desired.clone();
            return Ok(());
        }
        let source = capture_source(desired);
        let missing_codec = [
            self.running[0].is_some() && self.media.encoder.is_none(),
            self.running[1].is_some() && self.media.listening.is_none(),
        ];
        let [restart_mic, restart_playback] = stream_restarts(old, desired, missing_codec);
        self.media.audio.take();
        self.media.capture_block = None;
        self.media.capture_offset = 0;
        self.media.captured_len = 0;
        self.media.microphone_level = GainLimiter::new(desired.mic_gain);
        self.media.pending_send = None;
        for (stream, restart) in [
            (Stream::Mic, restart_mic),
            (Stream::Playback, restart_playback),
        ] {
            let wanted = if stream == Stream::Mic {
                desired.mic && !desired.muted
            } else {
                desired.playback
            };
            if self.running[stream.index()].is_some() && (!wanted || restart) {
                if !matches!(
                    self.rpc(Message::Stop { stream }).await?,
                    Message::Status { .. }
                ) {
                    return Err("desktop stop refused".into());
                }
                if stream == Stream::Mic {
                    self.media.encoder = None;
                } else {
                    self.media.listening = None;
                }
            }
        }
        self.media.settings = desired.clone();
        self.direction_errors = [None, None];
        let mut mic = desired.mic && !desired.muted;
        let mut playback = desired.playback;
        if !mic && !playback {
            return Ok(());
        }
        let sources = capture_sources(desired);
        let mut opened = Audio::open(mic, playback, desired.input, desired.output, sources[0]);
        if mic {
            for &fallback in &sources[1..] {
                if opened.is_ok() {
                    break;
                }
                opened = Audio::open(mic, playback, desired.input, desired.output, fallback);
            }
        }
        let mut audio = match opened {
            Ok(a) => a,
            Err(e) if mic && playback => {
                match Audio::open(true, false, desired.input, desired.output, source) {
                    Ok(a) => {
                        playback = false;
                        self.direction_errors[1] = Some(e.to_string());
                        a
                    }
                    Err(input_error) => {
                        mic = false;
                        self.direction_errors[0] = Some(input_error.to_string());
                        Audio::open(false, true, desired.input, desired.output, source)?
                    }
                }
            }
            Err(e) => return Err(e),
        };
        for (stream, available) in [(Stream::Mic, mic), (Stream::Playback, playback)] {
            if !available && self.running[stream.index()].is_some() {
                if !matches!(
                    self.rpc(Message::Stop { stream }).await?,
                    Message::Status { .. }
                ) {
                    return Err("desktop stop refused".into());
                }
                if stream == Stream::Mic {
                    self.media.encoder = None;
                } else {
                    self.media.listening = None;
                }
            }
        }
        for (stream, enabled) in [(Stream::Mic, mic), (Stream::Playback, playback)] {
            if !enabled || self.running[stream.index()].is_some() {
                continue;
            }
            self.epoch = self.epoch.checked_add(1).ok_or("epoch exhausted")?;
            let config = Config {
                stream,
                epoch: self.epoch,
            };
            match self.rpc(Message::Start(config)).await? {
                Message::Status { running, .. } if running[stream.index()] == Some(config) => {
                    if stream == Stream::Mic {
                        self.media.encoder = Some(Encoder::try_new(config)?);
                    } else {
                        self.media.listening = Some(config);
                    }
                }
                Message::Error { code } => {
                    self.direction_errors[stream.index()] = Some(format!("desktop: {code:?}"));
                    if stream == Stream::Mic {
                        mic = false;
                    } else {
                        playback = false;
                    }
                }
                _ => return Err("desktop did not apply stream configuration".into()),
            }
        }
        if mic != (desired.mic && !desired.muted) || playback != desired.playback {
            drop(audio);
            if !mic && !playback {
                let details = self
                    .direction_errors
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("no enabled direction could start: {details}").into());
            }
            audio = Audio::open(mic, playback, desired.input, desired.output, source)?;
        }
        audio.set_playback_gain(desired.playback_gain);
        audio.start()?;
        self.media.last_capture_at = Instant::now();
        self.media.settings.mic = mic;
        self.media.settings.playback = playback;
        self.media.audio = Some(audio);
        self.media.listen(self.media.listening)
    }
}

async fn connect(identity: &Identity) -> Result<Box<Connected>> {
    let bind = if identity.endpoint.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let mut endpoint = transport::client_endpoint(bind.parse()?)?;
    endpoint.set_default_client_config(transport::client(
        identity.desktop.clone(),
        identity.certificate.clone(),
        identity.private_key.clone(),
    )?);
    let connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(identity.endpoint, "litz.local")?,
    )
    .await
    .map_err(|_| Failure::ConnectTimeout)?
    .map_err(|error| -> transport::Error {
        if matches!(&error, quinn::ConnectionError::TransportError(e) if u64::from(e.code) == 0x178)
        {
            "Update Litz on both your phone and computer; their audio protocols differ".into()
        } else {
            error.into()
        }
    })?;
    // Root verification alone would permit another CA-signed leaf. The pairing
    // approval pins the exact certificate as well.
    if transport::peer_certificate(&connection)? != identity.desktop {
        return Err("desktop pin mismatch".into());
    }
    let control = Control::new(connection.open_bi().await?);
    let now = Instant::now();
    let mut c = Box::new(Connected {
        _endpoint: endpoint,
        control,
        request: 0,
        epoch: 0,
        revision: 0,
        running: [None, None],
        direction_errors: [None, None],
        desktop: None,
        owner: None,
        generation: 0,
        owned: false,
        media: Box::new(Media {
            connection,
            audio: None,
            encoder: None,
            listening: None,
            sender: MediaSender::default(),
            settings: Settings::default(),
            reduced: now,
            captured: [0.0; FRAME],
            microphone_rms: 0.0,
            last_capture_at: now,
            captured_len: 0,
            capture_block: None,
            capture_offset: 0,
            pending_send: None,
            microphone_level: GainLimiter::new(1.0),
            packet: [0; MAX_DATAGRAM],
            dropped: 0,
        }),
    });
    match c.rpc(Message::GetState {}).await? {
        Message::Status { .. } => {}
        Message::Error {
            code: Code::Unauthorized,
        } => return Err("Unauthorized".into()),
        Message::Error { code } => return Err(format!("desktop: {code:?}").into()),
        _ => return Err("invalid GetState response".into()),
    }
    c.rpc(Message::GetOwnership {}).await?;
    c.refresh_routes().await?;
    Ok(c)
}

fn status(
    identity: &Identity,
    desired: &Settings,
    c: Option<&Connected>,
    error: &Option<String>,
    reconnecting: bool,
    reconnect_allowed: bool,
) -> String {
    let audio = c.and_then(|c| c.media.audio.as_ref());
    let listening = audio
        .filter(|_| c.is_some_and(|c| c.media.listening.is_some()))
        .map(Audio::listening);
    serde_json::json!({
        "state": if reconnecting {"Reconnecting"} else if audio.is_some() {"Streaming"} else if c.is_some() {"Authenticated"} else {"Idle"},
        "reconnect_allowed": reconnect_allowed,
        "desktop": identity.endpoint.to_string(), "enrolled": !identity.desktop.is_empty(),
        "connected_endpoint": c.map(|connection| connection.media.connection.remote_address().to_string()),
        "desired": desired, "running": c.map(|c| [c.media.encoder.is_some() && audio.is_some(), c.media.listening.is_some() && audio.is_some()]).unwrap_or([false,false]),
        "revision": c.map(|c| c.revision), "direction_errors": c.map(|c| &c.direction_errors),
        "owner":c.and_then(|c|c.owner.as_ref()), "ownership_generation":c.map(|c|c.generation), "owned":c.is_some_and(|c|c.owned),
        "desktop_routes": c.and_then(|c| c.desktop.as_ref()),
        "applied": c.map(|c| &c.media.settings),
        "error": error,
        "microphone_rms":c.map(|c| c.media.microphone_rms),
        "audio":audio.map(Audio::info),
        "underruns": audio.map(|a| a.info().output_xruns),
        "callback_frames":audio.map(Audio::callback_frames),
        "rtt_ms": c.map(|c| c.media.connection.rtt().as_secs_f64()*1000.0),
        "jitter_ms":listening.as_ref().map(|l| l.buffer_ms), "late_packets": listening.as_ref().map(|l| l.late),
        "concealed_frames":listening.as_ref().map(|l| l.concealed), "transport_drops":c.map(|c|c.media.dropped),
    }).to_string()
}
#[derive(Debug, PartialEq)]
enum Failure {
    Cancelled,
    ConnectTimeout,
    ControlTimeout,
    AudioDiscontinuity,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "Stopped",
            Self::ConnectTimeout => "Computer is unavailable. Check that Litz is running on it.",
            Self::ControlTimeout => "Desktop control timed out",
            Self::AudioDiscontinuity => "Audio interrupted; recovering",
        })
    }
}
impl std::error::Error for Failure {}
fn retryable(mut error: &(dyn std::error::Error + 'static)) -> bool {
    loop {
        if let Some(connection) = error.downcast_ref::<quinn::ConnectionError>() {
            return matches!(
                connection,
                quinn::ConnectionError::Reset | quinn::ConnectionError::TimedOut
            ) || matches!(connection, quinn::ConnectionError::ApplicationClosed(close) if close.error_code == quinn::VarInt::from_u32(0));
        }
        if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
            )
        }) {
            return true;
        }
        if error.is::<tokio::time::error::Elapsed>()
            || matches!(
                error.downcast_ref::<Failure>(),
                Some(Failure::ControlTimeout | Failure::ConnectTimeout)
            )
        {
            return true;
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}
async fn run(
    mut identity: Identity,
    mut requests: tokio::sync::mpsc::Receiver<Request>,
    cancel: Arc<tokio::sync::Notify>,
    cancelled: Arc<AtomicBool>,
) {
    let mut desired = Settings::default();
    let mut connected: Option<Box<Connected>> = None;
    let mut handoff: Option<Handoff> = None;
    let mut handoff_target = None;
    let mut error = None;
    let mut reconnect_allowed = true;
    let mut retry: Option<tokio::time::Instant> = None;
    let mut attempt = 0u32;
    let mut audio_recoveries = 0u32;
    let mut retry_auto_route = false;
    let mut last_recovery = Instant::now();
    let mut health = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => {
                desired.mic = false; desired.playback = false;
                handoff = None; handoff_target = None;
                connected.take(); retry = None; retry_auto_route = false; error = None;
            }
            request = requests.recv() => {
                let Some(request) = request else { break; };
                if matches!(request.command, Command::Set { .. }) && cancelled.load(Ordering::Acquire) {
                    desired.mic = false; desired.playback = false; connected.take(); retry = None;
                    let _ = request.reply.send(status(&identity, &desired, None, &None, false, false));
                    continue;
                }
                match request.command {
                    Command::Status => {
                        if let Some(c) = connected.as_mut() && !c.owned {
                            error = c.rpc(Message::GetOwnership {}).await.err().map(|e| e.to_string());
                        }
                    },
                    Command::Address { endpoint } => {
                        if !endpoint.ip().is_unspecified() && !endpoint.ip().is_multicast() && endpoint.port() >= 1024 {
                            identity.endpoint = endpoint;
                            if handoff_target != Some(endpoint) {
                                handoff = None;
                                handoff_target = None;
                                if desired.mic || desired.playback {
                                    if connected.as_ref().is_some_and(|c| c.media.connection.remote_address() != endpoint) {
                                        let candidate = identity.clone();
                                        handoff_target = Some(endpoint);
                                        handoff = Some(Box::pin(async move { connect(&candidate).await }));
                                    } else if connected.is_none() {
                                        attempt = 0;
                                        retry = Some(tokio::time::Instant::now());
                                    }
                                } else if connected.as_ref().is_some_and(|c| c.media.connection.remote_address() != endpoint) {
                                    connected.take();
                                }
                            }
                        }
                    },
                    Command::RefreshRoutes => {
                        if let Some(c) = connected.as_mut() {
                            error = c.refresh_routes().await.err().map(|e| e.to_string());
                        }
                    },
                    Command::Connect => {
                        error = None;
                        reconnect_allowed = true;
                        if connected.is_none() {
                            match connect(&identity).await {
                                Ok(c) => connected = Some(c),
                                Err(e) => { reconnect_allowed = retryable(e.as_ref()); error = Some(e.to_string()); },
                            }
                        }
                    },
                    Command::Forget => {
                        handoff = None; handoff_target = None;
                        desired.mic = false; desired.playback = false; retry = None; retry_auto_route = false;
                        if let Some(c) = connected.as_mut() { let _ = c.rpc(Message::Unpair {}).await; }
                        connected.take(); error = None;
                    },
                    Command::Disconnect => {
                        handoff = None; handoff_target = None;
                        desired.mic = false; desired.playback = false; retry = None; retry_auto_route = false;
                        connected.take(); error = None;
                    },
                    Command::Route { expected_revision, route } => {
                        let result = match connected.as_mut() {
                            Some(c) => c.route(expected_revision, route, &desired).await,
                            None => Err("Connect to the desktop first".into()),
                        };
                        match result {
                            Ok(reason) => error = reason,
                            Err(failure) => {
                                reconnect_allowed = retryable(failure.as_ref()) || failure.downcast_ref::<Failure>() == Some(&Failure::AudioDiscontinuity);
                                error = Some(failure.to_string()); connected.take();
                                retry = (reconnect_allowed && desired.reconnect && (desired.mic || desired.playback))
                                    .then(|| tokio::time::Instant::now() + Duration::from_millis(500));
                            }
                        }
                    },
                    Command::Voice { expected_revision, voice } => {
                        let result = match connected.as_mut() {
                            Some(c) => c.voice(expected_revision, voice).await,
                            None => Err("Connect to the computer first".into()),
                        };
                        match result {
                            Ok(reason) => error = reason,
                            Err(failure) => {
                                reconnect_allowed = retryable(failure.as_ref()) || failure.downcast_ref::<Failure>() == Some(&Failure::AudioDiscontinuity);
                                error = Some(failure.to_string()); connected.take();
                                retry = (reconnect_allowed && desired.reconnect && (desired.mic || desired.playback))
                                    .then(|| tokio::time::Instant::now() + Duration::from_millis(500));
                            }
                        }
                    },
                    Command::Stop => {
                        handoff = None; handoff_target = None;
                        desired.mic = false; desired.playback = false; retry = None; retry_auto_route = false;
                        error = None;
                        if let Some(c) = connected.as_mut() && let Err(failure) = c.release().await {
                            error = Some(failure.to_string());
                            connected.take(); // a failed release must not retain ownership
                        }
                    }
                    Command::Set { settings } => {
                        let operation = async {
                            settings.validate()?;
                            desired = settings;
                            retry = None; retry_auto_route = false; attempt = 0; error = None;
                            if !desired.mic && !desired.playback {
                                handoff = None; handoff_target = None;
                                if let Some(c) = connected.as_mut() { c.release().await?; c.media.settings = desired.clone(); }
                                return Ok(());
                            }
                            if connected.as_ref().is_some_and(|c| c.media.connection.remote_address() != identity.endpoint) {
                                connected.take();
                            }
                            if connected.is_none() {
                                connected = Some(connect(&identity).await?);
                            }
                            if let Some(connection) = connected.as_mut() {
                                connection.apply(&desired).await?;
                            }
                            Ok(())
                        };
                        let result = tokio::select! {
                            biased;
                            _ = cancel.notified() => Err(Failure::Cancelled.into()),
                            result = operation => result,
                        };
                        if result.as_ref().is_err_and(|e: &transport::Error|e.downcast_ref::<Failure>() == Some(&Failure::Cancelled)) {
                            desired.mic = false; desired.playback = false;
                        }
                        desired.takeover = None;
                        if let Err(e) = result {
                            reconnect_allowed = retryable(e.as_ref());
                            error = Some(e.to_string());
                            if connected.as_ref().is_some_and(|c| !c.owned) {
                                desired.mic = false; desired.playback = false;
                            } else { connected.take(); }
                            if desired.reconnect
                                && (desired.mic || desired.playback)
                                && retryable(e.as_ref())
                            {
                                attempt = 0;
                                retry = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                            }
                        }
                    }
                }
                let _ = request.reply.send(status(&identity, &desired, connected.as_deref(), &error, retry.is_some(), reconnect_allowed));
            }
            result = async { handoff.as_mut().unwrap().await }, if handoff.is_some() => {
                handoff = None;
                let target = handoff_target.take();
                if target != Some(identity.endpoint) || !(desired.mic || desired.playback) {
                    continue;
                }
                if let Ok(mut replacement) = result {
                    connected.take();
                    match replacement.apply(&desired).await {
                        Ok(()) => {
                            connected = Some(replacement);
                            error = None; retry = None; attempt = 0; reconnect_allowed = true;
                        }
                        Err(failure) => {
                            reconnect_allowed = retryable(failure.as_ref());
                            error = Some(failure.to_string());
                            retry = (reconnect_allowed && desired.reconnect)
                                .then(|| tokio::time::Instant::now() + Duration::from_millis(500));
                        }
                    }
                }
            }
            result = async { connected.as_mut().unwrap().event().await }, if connected.is_some() => {
                if let Err(e) = result {
                    if last_recovery.elapsed() > Duration::from_secs(10) { audio_recoveries = 0; }
                    if e.downcast_ref::<Failure>() == Some(&Failure::AudioDiscontinuity) && audio_recoveries < 3 {
                        audio_recoveries += 1;
                        last_recovery = Instant::now();
                        if let Some(connection) = connected.as_mut() {
                            connection.media.clear();
                            if connection.apply(&desired).await.is_ok() {
                                error = None;
                                continue;
                            }
                        }
                    }
                    let close_code = connected.as_ref().and_then(|c| match c.media.connection.close_reason() {
                        Some(quinn::ConnectionError::ApplicationClosed(close)) => Some(close.error_code),
                        _ => None,
                    });
                    let moved = close_code == Some(quinn::VarInt::from_u32(0x20));
                    let deliberate = moved || close_code == Some(quinn::VarInt::from_u32(0x21));
                    if deliberate { desired.mic = false; desired.playback = false; retry = None; }
                    let network_failure = !deliberate && (retryable(e.as_ref())
                        || e.downcast_ref::<Failure>() == Some(&Failure::AudioDiscontinuity)
                        || connected.as_ref().and_then(|c|c.media.connection.close_reason()).is_some_and(|e| retryable(&e)));
                    reconnect_allowed = network_failure;
                    error = Some(e.to_string()); connected.take();
                    if moved && let Ok(observer) = connect(&identity).await {
                        connected = Some(observer);
                        error = None;
                    }
                    if network_failure && desired.reconnect && (desired.mic || desired.playback) {
                        attempt = 0;
                        retry = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                    }
                }
            }
            _ = health.tick() => {
                if connected.as_ref().is_some_and(|c| c.media.audio.as_ref().is_some_and(Audio::fault)) {
                    reconnect_allowed = false;
                    let c = connected.as_mut().unwrap();
                    let faults = c.media.audio.as_ref().map(|a| a.info().error).unwrap_or(3);
                    if faults == 0 {
                        continue;
                    } else {
                        if faults & 1 != 0 && desired.input != 0 { desired.mic = false; }
                        if faults & 2 != 0 && desired.output != 0 { desired.playback = false; }
                        match c.apply(&desired).await {
                            Ok(()) => {
                                reconnect_allowed = true;
                                if faults & 1 != 0 && desired.input != 0 { c.direction_errors[0] = Some("Input route disconnected; select and start again".into()); }
                                if faults & 2 != 0 && desired.output != 0 { c.direction_errors[1] = Some("Output route disconnected; select and start again".into()); }
                            }
                            Err(e) => {
                                error = Some(e.to_string());
                                connected.take();
                                retry_auto_route = (faults & 1 != 0 && desired.mic && desired.input == 0)
                                    || (faults & 2 != 0 && desired.playback && desired.output == 0);
                                reconnect_allowed = retry_auto_route || retryable(e.as_ref());
                                if desired.reconnect
                                    && (desired.mic || desired.playback)
                                    && reconnect_allowed
                                {
                                    attempt = 0;
                                    retry = Some(tokio::time::Instant::now() + Duration::from_millis(500));
                                }
                            }
                        }
                    }
                }
                if retry.is_some_and(|at| at <= tokio::time::Instant::now()) {
                    let operation = async {
                        let mut c = connect(&identity).await?;
                        c.apply(&desired).await?;
                        Ok::<_, transport::Error>(c)
                    };
                    let result = tokio::select! {
                        biased;
                        _ = cancel.notified() => Err(Failure::Cancelled.into()),
                        result = operation => result,
                    };
                    match result {
                        Ok(c) => { connected = Some(c); error = None; retry = None; retry_auto_route = false; attempt = 0; reconnect_allowed = true; }
                        Err(e) => {
                            if e.downcast_ref::<Failure>() == Some(&Failure::Cancelled) {
                                desired.mic = false; desired.playback = false; retry = None; continue;
                            }
                            error = Some(e.to_string()); attempt = (attempt + 1).min(5);
                            if !retry_auto_route && !retryable(e.as_ref()) {
                                retry = None;
                                continue;
                            }
                            let mut random = [0u8;2]; let _ = getrandom::fill(&mut random);
                            retry = Some(tokio::time::Instant::now() + Duration::from_millis((500u64 << attempt).min(15000) + u16::from_ne_bytes(random) as u64 % 250));
                        }
                    }
                }
            }
        }
    }
    drop(connected);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmute_reuses_duplex_audio_without_a_microphone_encoder() {
        let muted = Settings {
            mic: true,
            playback: true,
            muted: true,
            ..Settings::default()
        };
        let unmuted = Settings {
            muted: false,
            ..muted.clone()
        };
        assert!(can_reuse_audio(&muted, &unmuted, false, true));
        assert!(can_reuse_audio(&unmuted, &muted, true, true));
        assert!(!can_reuse_audio(&unmuted, &unmuted, false, true));
        assert!(!can_reuse_audio(&muted, &unmuted, false, false));
    }

    #[test]
    fn changing_one_direction_keeps_the_other_wire_stream() {
        let listening = Settings {
            playback: true,
            raw_supported: true,
            ..Settings::default()
        };
        assert_eq!(
            stream_restarts(
                &listening,
                &Settings {
                    mic: true,
                    ..listening.clone()
                },
                [false, false],
            ),
            [true, false]
        );
        let duplex = Settings {
            mic: true,
            ..listening.clone()
        };
        assert_eq!(
            stream_restarts(&duplex, &listening, [false, false]),
            [false, false]
        );
        assert_eq!(
            stream_restarts(
                &duplex,
                &Settings {
                    output: 12,
                    ..duplex.clone()
                },
                [false, false],
            ),
            [false, true]
        );
        assert_eq!(
            stream_restarts(
                &duplex,
                &Settings {
                    source: 5,
                    ..duplex.clone()
                },
                [false, false],
            ),
            [true, false]
        );
        let microphone = Settings {
            mic: true,
            ..Settings::default()
        };
        assert_eq!(
            stream_restarts(
                &microphone,
                &Settings {
                    source: 5,
                    ..microphone.clone()
                },
                [false, false],
            ),
            [true, false]
        );
        assert_eq!(
            stream_restarts(&duplex, &duplex, [true, true]),
            [true, true]
        );
        assert_eq!(
            stream_restarts(&microphone, &microphone, [true, false]),
            [true, false]
        );
        let selected = Settings {
            source: 5,
            ..Settings::default()
        };
        assert_eq!(capture_source(&selected), 5);
        assert_eq!(
            capture_source(&Settings {
                playback: true,
                ..selected.clone()
            }),
            5
        );
        assert_eq!(
            capture_source(&Settings {
                source: 0,
                raw_supported: true,
                ..selected
            }),
            9
        );
    }

    #[test]
    fn connection_and_reconnect_futures_keep_pcm_off_the_stack() {
        let identity = crate::fresh_identity().unwrap();
        let (_, requests) = tokio::sync::mpsc::channel(2);
        let future = run(
            identity,
            requests,
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(std::mem::size_of::<Connected>() < 4096);
        assert!(
            std::mem::size_of_val(&future) < 64 * 1024,
            "engine future is {} bytes",
            std::mem::size_of_val(&future)
        );
    }

    #[test]
    fn offline_start_stop_and_close_do_not_panic() {
        let mut identity = crate::fresh_identity().unwrap();
        identity.desktop = identity.certificate.clone();
        identity.endpoint = "127.0.0.1:9".parse().unwrap();
        let engine = Engine::try_new(identity).unwrap();
        for (mic, playback, reconnect) in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let settings = Settings {
                mic,
                playback,
                reconnect,
                ..Settings::default()
            };
            let result = engine
                .command(&serde_json::json!({ "op": "set", "settings": settings }).to_string())
                .unwrap();
            let status: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert_eq!(status["running"], serde_json::json!([false, false]));
            assert!(status["error"].is_string());
            if reconnect {
                assert_eq!(status["state"], "Reconnecting");
                assert_eq!(status["reconnect_allowed"], true);
            }
            let result = engine.command(r#"{"op":"stop"}"#).unwrap();
            let status: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert_eq!(status["desired"]["mic"], false);
            assert_eq!(status["desired"]["playback"], false);
            assert_eq!(status["state"], "Idle");
        }
        engine.cancel();
        drop(engine);
    }
    #[test]
    fn capture_sources_returns_expected_sources() {
        assert_eq!(
            capture_sources(&Settings {
                playback: true,
                ..Default::default()
            }),
            vec![6, 1]
        );
        assert_eq!(
            capture_sources(&Settings {
                playback: true,
                raw_supported: true,
                ..Default::default()
            }),
            vec![9, 6, 1]
        );
        assert_eq!(
            capture_sources(&Settings {
                playback: true,
                source: 7,
                ..Default::default()
            }),
            vec![7]
        );
    }
    #[tokio::test]
    async fn outer_connection_deadline_is_retryable() {
        let elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .unwrap_err();
        assert!(retryable(&elapsed));
    }
    #[test]
    fn handoff_or_revocation_is_never_a_transport_retry() {
        for code in [1, 0x20, 0x21, 0x22] {
            let closed = quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
                error_code: quinn::VarInt::from_u32(code),
                reason: b"connection timed out".as_slice().into(),
            });
            assert!(!retryable(&closed));
            assert!(!retryable(&quinn::ReadError::ConnectionLost(closed)));
        }
        assert!(retryable(&quinn::ConnectionError::TimedOut));
        assert!(retryable(&quinn::WriteError::ConnectionLost(
            quinn::ConnectionError::Reset
        )));
        assert!(!retryable(&Failure::Cancelled));
        assert!(retryable(&Failure::ControlTimeout));
    }
    #[test]
    fn defaults_are_stopped_raw_and_gains_are_bounded() {
        let s = Settings::default();
        assert!(!s.mic && !s.playback && s.reconnect);
        assert_eq!(gain(0.25, 2.0), 0.5);
        assert_eq!(gain(f32::NAN, 1.0), 0.0);
        assert_eq!(gain(0.9, 2.0), 1.0);
        assert!(
            Settings {
                mic_gain: f32::INFINITY,
                ..s
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn every_recording_mode_has_explicit_capture_policy() {
        for source in [0, 1, 5, 6, 7, 9, 10] {
            for playback in [false, true] {
                for raw_supported in [false, true] {
                    let settings = Settings {
                        mic: true,
                        source,
                        playback,
                        raw_supported,
                        ..Default::default()
                    };
                    settings.validate().unwrap();
                    let sources = capture_sources(&settings);
                    if source != 0 {
                        assert_eq!(sources, vec![source]);
                    } else {
                        assert_eq!(sources[0], if raw_supported { 9 } else { 6 });
                        assert_eq!(*sources.last().unwrap(), 1);
                    }
                }
            }
        }
    }
}
