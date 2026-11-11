use crate::Identity;
use crate::audio::{Audio, AudioInfo, GainLimiter, ListeningStats};
use crate::failure::{Failure, cause};
use litz_core::media::codec::Encoder;
use litz_core::media::receiver::Receiver;
use litz_core::protocol::{
    Code, Config, DesktopRoutes, FRAME, MAX_DATAGRAM, Message, Record, Route, Stream,
    VoiceSettings, close,
};
use litz_core::transport::{self, Control, MediaSender, Result};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Settings {
    #[uniffi(default)]
    pub mic: bool,
    #[uniffi(default)]
    pub playback: bool,
    #[uniffi(default)]
    pub muted: bool,
    #[uniffi(default = 1.0)]
    pub mic_gain: f32,
    #[uniffi(default = 1.0)]
    pub playback_gain: f32,
    #[uniffi(default)]
    pub input: i32,
    #[uniffi(default)]
    pub source: i32,
    #[uniffi(default)]
    pub output: i32,
    #[uniffi(default = true)]
    pub reconnect: bool,
    #[uniffi(default)]
    pub takeover: Option<u64>,
    #[uniffi(default)]
    pub raw_supported: bool,
    #[uniffi(default)]
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
enum Command {
    Status,
    Connect,
    RefreshRoutes,
    Disconnect,
    Forget,
    Address {
        endpoint: SocketAddr,
    },
    Route {
        expected_revision: u64,
        route: Route,
    },
    Voice {
        expected_revision: u64,
        voice: VoiceSettings,
    },
    Set {
        settings: Settings,
    },
    Stop,
}
struct Request {
    command: Command,
    reply: mpsc::SyncSender<Status>,
}

#[derive(Clone, Copy, Debug, PartialEq, uniffi::Enum)]
pub enum SessionState {
    Idle,
    Authenticated,
    Streaming,
    Reconnecting,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct Direction {
    pub running: bool,
    pub error: Option<Failure>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct Status {
    pub state: SessionState,
    pub reconnect_allowed: bool,
    pub revoked: bool,
    pub enrolled: bool,
    pub desktop: SocketAddr,
    pub connected_endpoint: Option<SocketAddr>,
    pub desired: Settings,
    pub microphone: Direction,
    pub listening: Direction,
    pub owner: Option<String>,
    pub ownership_generation: Option<u64>,
    pub owned: bool,
    pub desktop_routes: Option<DesktopRoutes>,
    pub error: Option<Failure>,
    pub microphone_rms: Option<f32>,
    pub audio: Option<AudioInfo>,
    pub callback_frames: Option<u64>,
    pub rtt_ms: Option<f64>,
    pub listening_stats: Option<ListeningStats>,
    pub transport_drops: Option<u64>,
}

#[derive(uniffi::Object)]
pub struct Engine {
    tx: Option<tokio::sync::mpsc::Sender<Request>>,
    worker: Option<std::thread::JoinHandle<()>>,
    cancel: Arc<tokio::sync::Notify>,
    cancelled: Arc<AtomicBool>,
}
#[uniffi::export]
impl Engine {
    #[uniffi::constructor]
    pub fn new(identity: Identity) -> std::result::Result<Arc<Self>, Failure> {
        Self::start(identity)
            .map(Arc::new)
            .map_err(|error| Failure::of(error.as_ref()))
    }

    /// Stops pending work at once, from any thread, even mid-command.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.notify_one();
    }

    pub fn status(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::Status)
    }

    pub fn connect(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::Connect)
    }

    pub fn refresh_routes(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::RefreshRoutes)
    }

    pub fn disconnect(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::Disconnect)
    }

    pub fn forget(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::Forget)
    }

    pub fn address(&self, endpoint: SocketAddr) -> std::result::Result<Status, Failure> {
        self.request(Command::Address { endpoint })
    }

    pub fn route(
        &self,
        expected_revision: u64,
        route: Route,
    ) -> std::result::Result<Status, Failure> {
        self.request(Command::Route {
            expected_revision,
            route,
        })
    }

    pub fn voice(
        &self,
        expected_revision: u64,
        voice: VoiceSettings,
    ) -> std::result::Result<Status, Failure> {
        self.request(Command::Voice {
            expected_revision,
            voice,
        })
    }

    pub fn set(&self, settings: Settings) -> std::result::Result<Status, Failure> {
        self.request(Command::Set { settings })
    }

    pub fn stop(&self) -> std::result::Result<Status, Failure> {
        self.request(Command::Stop)
    }
}

impl Engine {
    fn start(identity: Identity) -> Result<Self> {
        #[cfg(target_os = "android")]
        crate::oboe::log_panics();
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
                crate::oboe::raise_thread_priority();
                runtime.block_on(Box::pin(run(identity, rx, stopped, worker_cancelled)));
            })?;
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
            cancel,
            cancelled,
        })
    }
    fn request(&self, command: Command) -> std::result::Result<Status, Failure> {
        let closed = || Failure::Internal {
            detail: "engine closed".into(),
        };
        let (reply, result) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .ok_or_else(closed)?
            .blocking_send(Request { command, reply })
            .map_err(|_| closed())?;
        result
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::ControlTimeout)
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
                return Err(Failure::AudioDeviceDisconnected.into());
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
                if self.capture_block.as_ref().is_some_and(|block| block.gap) {
                    self.captured_len = 0;
                }
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
    direction_errors: [Option<Failure>; 2],
    desktop: Option<DesktopRoutes>,
    owner: Option<String>,
    generation: u64,
    owned: bool,
}
type Handoff = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Box<Connected>>>>>;
impl Drop for Connected {
    fn drop(&mut self) {
        self.media.clear();
        self.media.connection.close(close::ENDED, b"phone stopped");
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
                        self.direction_errors[1] = Some(Failure::ComputerRefused {
                            reason: Code::RouteConflict,
                        });
                        self.refresh_routes().await?;
                        Ok(())
                    }
                    Ok(_) => Err(Failure::UnexpectedReply.into()),
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
                        return Err(Failure::UnexpectedReply.into());
                    }
                    if record.request_id != self.request {
                        return Err(Failure::UnexpectedReply.into());
                    }
                    if let Message::Status { revision, running } = &record.message {
                        self.revision = *revision;
                        self.running = *running;
                    }
                    if let Message::Ownership { generation, owner, owned } = &record.message {
                        self.generation = *generation;
                        self.owner = owner.clone();
                        self.owned = *owned;
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
            _ => Err(Failure::UnexpectedReply.into()),
        }
    }
    async fn route(
        &mut self,
        expected_revision: u64,
        route: Route,
        desired: &Settings,
    ) -> Result<Option<Failure>> {
        self.acquire(None).await?;
        self.refresh_routes().await?;
        if expected_revision != self.revision {
            return Ok(Some(Failure::OutputsChanged));
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
                        self.direction_errors[1] = Some(Failure::ComputerRefused { reason: code });
                    }
                    _ => return Err(Failure::UnexpectedReply.into()),
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
            Message::Error { code } => Some(Failure::ComputerRefused { reason: code }),
            _ => Some(Failure::UnexpectedReply),
        })
    }
    async fn voice(
        &mut self,
        expected_revision: u64,
        voice: litz_core::protocol::VoiceSettings,
    ) -> Result<Option<Failure>> {
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
            Message::Error { code } => Some(Failure::ComputerRefused { reason: code }),
            _ => Some(Failure::UnexpectedReply),
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
                return Err(Failure::UnexpectedReply.into());
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
                return Err(Failure::InUse {
                    owner: self.owner.clone(),
                }
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
                        return Err(Failure::UnexpectedReply.into());
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
                    return Err(Failure::UnexpectedReply.into());
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
                        self.direction_errors[1] = Some(Failure::of(e.as_ref()));
                        a
                    }
                    Err(input_error) => {
                        mic = false;
                        self.direction_errors[0] = Some(Failure::of(input_error.as_ref()));
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
                    return Err(Failure::UnexpectedReply.into());
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
                    self.direction_errors[stream.index()] =
                        Some(Failure::ComputerRefused { reason: code });
                    if stream == Stream::Mic {
                        mic = false;
                    } else {
                        playback = false;
                    }
                }
                _ => return Err(Failure::UnexpectedReply.into()),
            }
        }
        if mic != (desired.mic && !desired.muted) || playback != desired.playback {
            drop(audio);
            if !mic && !playback {
                return Err(Failure::Refused.into());
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
        if matches!(&error, quinn::ConnectionError::TransportError(e) if u64::from(e.code) == close::NO_COMMON_PROTOCOL)
        {
            Failure::ProtocolMismatch.into()
        } else {
            error.into()
        }
    })?;
    // Root verification alone would permit another CA-signed leaf. The pairing
    // approval pins the exact certificate as well.
    if transport::peer_certificate(&connection)? != identity.desktop {
        return Err(Failure::IdentityChanged.into());
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
        } => return Err(Failure::Revoked.into()),
        Message::Error { code } => return Err(Failure::ComputerRefused { reason: code }.into()),
        _ => return Err(Failure::UnexpectedReply.into()),
    }
    c.rpc(Message::GetOwnership {}).await?;
    c.refresh_routes().await?;
    Ok(c)
}

fn retryable(error: &(dyn std::error::Error + 'static)) -> bool {
    for e in std::iter::successors(Some(error), |e| e.source()) {
        if let Some(connection) = e.downcast_ref::<quinn::ConnectionError>() {
            return match connection {
                quinn::ConnectionError::Reset | quinn::ConnectionError::TimedOut => true,
                quinn::ConnectionError::ApplicationClosed(closed) => {
                    closed.error_code == close::ENDED
                }
                _ => false,
            };
        }
        let transient_io = e.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
            )
        });
        if transient_io
            || e.is::<tokio::time::error::Elapsed>()
            || matches!(
                e.downcast_ref(),
                Some(Failure::ControlTimeout | Failure::ConnectTimeout)
            )
        {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Recovery {
    Automatic,
    Manual,
    PairAgain,
}
impl Recovery {
    fn after(error: &(dyn std::error::Error + 'static), retry_anyway: bool) -> Self {
        let forgotten = match cause::<quinn::ConnectionError>(error) {
            Some(quinn::ConnectionError::ApplicationClosed(closed)) => {
                closed.error_code == close::FORGOTTEN
            }
            _ => false,
        };
        if forgotten || cause::<Failure>(error) == Some(&Failure::Revoked) {
            Self::PairAgain
        } else if retry_anyway || retryable(error) {
            Self::Automatic
        } else {
            Self::Manual
        }
    }
}

enum Event {
    Cancelled,
    Request(Option<Request>),
    HandedOff(Result<Box<Connected>>),
    Connection(Result<()>),
    Tick,
}

struct Supervisor {
    identity: Identity,
    desired: Settings,
    connected: Option<Box<Connected>>,
    handoff: Option<(std::net::SocketAddr, Handoff)>,
    error: Option<Failure>,
    recovery: Recovery,
    retry: Option<tokio::time::Instant>,
    attempt: u32,
    retry_auto_route: bool,
    audio_recoveries: u32,
    last_recovery: Instant,
}

impl Supervisor {
    fn new(identity: Identity) -> Self {
        Self {
            identity,
            desired: Settings::default(),
            connected: None,
            handoff: None,
            error: None,
            recovery: Recovery::Automatic,
            retry: None,
            attempt: 0,
            retry_auto_route: false,
            audio_recoveries: 0,
            last_recovery: Instant::now(),
        }
    }

    fn active(&self) -> bool {
        self.desired.mic || self.desired.playback
    }

    fn stop_wanting_audio(&mut self) {
        self.desired.mic = false;
        self.desired.playback = false;
        self.handoff = None;
        self.retry = None;
        self.retry_auto_route = false;
    }

    fn retry_soon(&mut self) {
        self.attempt = 0;
        self.retry = Some(tokio::time::Instant::now() + Duration::from_millis(500));
    }

    fn connected(&mut self, connection: Box<Connected>) {
        self.connected = Some(connection);
        self.error = None;
        self.recovery = Recovery::Automatic;
        self.retry = None;
        self.retry_auto_route = false;
        self.attempt = 0;
    }

    fn lost(&mut self, error: &transport::Error, retry_anyway: bool) {
        self.recovery = Recovery::after(error.as_ref(), retry_anyway);
        self.error = Some(Failure::of(error.as_ref()));
        self.connected = None;
        if self.recovery == Recovery::Automatic && self.desired.reconnect && self.active() {
            self.retry_soon();
        }
    }

    fn status(&self) -> Status {
        let c = self.connected.as_deref();
        let audio = c.and_then(|c| c.media.audio.as_ref());
        let state = if self.retry.is_some() {
            SessionState::Reconnecting
        } else if audio.is_some() {
            SessionState::Streaming
        } else if c.is_some() {
            SessionState::Authenticated
        } else {
            SessionState::Idle
        };
        let direction = |running: fn(&Media) -> bool, index: usize| Direction {
            running: c.is_some_and(|c| running(&c.media) && c.media.audio.is_some()),
            error: c.and_then(|c| c.direction_errors[index].clone()),
        };
        Status {
            state,
            reconnect_allowed: self.recovery == Recovery::Automatic,
            revoked: self.recovery == Recovery::PairAgain,
            enrolled: !self.identity.desktop.is_empty(),
            desktop: self.identity.endpoint,
            connected_endpoint: c.map(|c| c.media.connection.remote_address()),
            desired: self.desired.clone(),
            microphone: direction(|media| media.encoder.is_some(), 0),
            listening: direction(|media| media.listening.is_some(), 1),
            owner: c.and_then(|c| c.owner.clone()),
            ownership_generation: c.map(|c| c.generation),
            owned: c.is_some_and(|c| c.owned),
            desktop_routes: c.and_then(|c| c.desktop.clone()),
            error: self.error.clone(),
            microphone_rms: c.map(|c| c.media.microphone_rms),
            audio: audio.map(Audio::info),
            callback_frames: audio.map(Audio::callback_frames),
            rtt_ms: c.map(|c| c.media.connection.rtt().as_secs_f64() * 1000.0),
            listening_stats: audio
                .filter(|_| c.is_some_and(|c| c.media.listening.is_some()))
                .map(Audio::listening),
            transport_drops: c.map(|c| c.media.dropped),
        }
    }

    async fn next(
        &mut self,
        requests: &mut tokio::sync::mpsc::Receiver<Request>,
        cancel: &tokio::sync::Notify,
        health: &mut tokio::time::Interval,
    ) -> Event {
        let handoff = async {
            match &mut self.handoff {
                Some((_, connecting)) => connecting.await,
                None => std::future::pending().await,
            }
        };
        let connection = async {
            match &mut self.connected {
                Some(c) => c.event().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = cancel.notified() => Event::Cancelled,
            request = requests.recv() => Event::Request(request),
            result = handoff => Event::HandedOff(result),
            result = connection => Event::Connection(result),
            _ = health.tick() => Event::Tick,
        }
    }

    fn cancel(&mut self) {
        self.stop_wanting_audio();
        self.connected = None;
        self.error = None;
    }

    fn shut_down(&mut self) {
        self.cancel();
        self.recovery = Recovery::Manual;
    }

    async fn command(&mut self, command: Command, cancel: &tokio::sync::Notify) {
        match command {
            Command::Status => {
                if let Some(c) = self.connected.as_mut()
                    && !c.owned
                {
                    self.error = c
                        .rpc(Message::GetOwnership {})
                        .await
                        .err()
                        .map(|e| Failure::of(e.as_ref()));
                }
            }
            Command::Address { endpoint } => self.address(endpoint),
            Command::RefreshRoutes => {
                if let Some(c) = self.connected.as_mut() {
                    self.error = c
                        .refresh_routes()
                        .await
                        .err()
                        .map(|e| Failure::of(e.as_ref()));
                }
            }
            Command::Connect => {
                self.error = None;
                self.recovery = Recovery::Automatic;
                if self.connected.is_none() {
                    match connect(&self.identity).await {
                        Ok(c) => self.connected(c),
                        Err(e) => {
                            self.recovery = Recovery::after(e.as_ref(), false);
                            self.error = Some(Failure::of(e.as_ref()));
                        }
                    }
                }
            }
            Command::Forget => {
                self.stop_wanting_audio();
                if let Some(c) = self.connected.as_mut() {
                    let _ = c.rpc(Message::Unpair {}).await;
                }
                self.connected = None;
                self.error = None;
            }
            Command::Disconnect => {
                self.stop_wanting_audio();
                self.connected = None;
                self.error = None;
            }
            Command::Route {
                expected_revision,
                route,
            } => {
                let result = match self.connected.as_mut() {
                    Some(c) => c.route(expected_revision, route, &self.desired).await,
                    None => Err(Failure::NotConnected.into()),
                };
                self.settle(result);
            }
            Command::Voice {
                expected_revision,
                voice,
            } => {
                let result = match self.connected.as_mut() {
                    Some(c) => c.voice(expected_revision, voice).await,
                    None => Err(Failure::NotConnected.into()),
                };
                self.settle(result);
            }
            Command::Stop => {
                self.stop_wanting_audio();
                self.error = None;
                if let Some(c) = self.connected.as_mut()
                    && let Err(e) = c.release().await
                {
                    self.error = Some(Failure::of(e.as_ref()));
                    // A failed release must not keep ownership.
                    self.connected = None;
                }
            }
            Command::Set { settings } => self.set(settings, cancel).await,
        }
    }

    fn address(&mut self, endpoint: std::net::SocketAddr) {
        if endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() || endpoint.port() < 1024
        {
            return;
        }
        self.identity.endpoint = endpoint;
        if self
            .handoff
            .as_ref()
            .is_some_and(|(target, _)| *target == endpoint)
        {
            return;
        }
        self.handoff = None;
        let elsewhere = self
            .connected
            .as_ref()
            .is_some_and(|c| c.media.connection.remote_address() != endpoint);
        if !self.active() {
            if elsewhere {
                self.connected = None;
            }
        } else if elsewhere {
            let candidate = self.identity.clone();
            self.handoff = Some((endpoint, Box::pin(async move { connect(&candidate).await })));
        } else if self.connected.is_none() {
            self.attempt = 0;
            self.retry = Some(tokio::time::Instant::now());
        }
    }

    fn settle(&mut self, result: Result<Option<Failure>>) {
        match result {
            Ok(notice) => self.error = notice,
            Err(e) => {
                let discontinuity =
                    cause::<Failure>(e.as_ref()) == Some(&Failure::AudioDiscontinuity);
                self.lost(&e, discontinuity);
            }
        }
    }

    async fn set(&mut self, settings: Settings, cancel: &tokio::sync::Notify) {
        let result = tokio::select! {
            biased;
            _ = cancel.notified() => Err(Failure::Cancelled.into()),
            result = self.apply_settings(settings) => result,
        };
        self.desired.takeover = None;
        let Err(e) = result else {
            self.recovery = Recovery::Automatic;
            return;
        };
        match cause::<Failure>(e.as_ref()) {
            Some(Failure::Cancelled) => {
                self.shut_down();
                self.error = Some(Failure::of(e.as_ref()));
            }
            Some(Failure::Refused) => {
                self.stop_wanting_audio();
                self.recovery = Recovery::Manual;
                self.error = Some(Failure::of(e.as_ref()));
                if let Some(c) = self.connected.as_mut()
                    && c.release().await.is_err()
                {
                    self.connected = None;
                }
            }
            _ if self.connected.as_ref().is_some_and(|c| !c.owned) => {
                self.stop_wanting_audio();
                self.recovery = Recovery::after(e.as_ref(), false);
                self.error = Some(Failure::of(e.as_ref()));
            }
            _ => self.lost(&e, false),
        }
    }

    async fn apply_settings(&mut self, settings: Settings) -> Result<()> {
        settings.validate()?;
        self.desired = settings;
        self.retry = None;
        self.retry_auto_route = false;
        self.attempt = 0;
        self.error = None;
        if !self.active() {
            self.handoff = None;
            if let Some(c) = self.connected.as_mut() {
                c.release().await?;
                c.media.settings = self.desired.clone();
            }
            return Ok(());
        }
        if self
            .connected
            .as_ref()
            .is_some_and(|c| c.media.connection.remote_address() != self.identity.endpoint)
        {
            self.connected = None;
        }
        if self.connected.is_none() {
            self.connected = Some(connect(&self.identity).await?);
        }
        if let Some(c) = self.connected.as_mut() {
            c.apply(&self.desired).await?;
        }
        Ok(())
    }

    async fn handed_off(&mut self, result: Result<Box<Connected>>) {
        let Some((target, _)) = self.handoff.take() else {
            return;
        };
        if target != self.identity.endpoint || !self.active() {
            return;
        }
        let Ok(mut replacement) = result else {
            return;
        };
        self.connected = None;
        match replacement.apply(&self.desired).await {
            Ok(()) => self.connected(replacement),
            Err(e) => self.lost(&e, false),
        }
    }

    async fn connection_failed(&mut self, error: transport::Error) {
        if self.last_recovery.elapsed() > Duration::from_secs(10) {
            self.audio_recoveries = 0;
        }
        let discontinuity = cause::<Failure>(error.as_ref()) == Some(&Failure::AudioDiscontinuity);
        if discontinuity && self.audio_recoveries < 3 {
            self.audio_recoveries += 1;
            self.last_recovery = Instant::now();
            if let Some(c) = self.connected.as_mut() {
                c.media.clear();
                if c.apply(&self.desired).await.is_ok() {
                    self.error = None;
                    return;
                }
            }
        }
        let closed = self
            .connected
            .as_ref()
            .and_then(|c| c.media.connection.close_reason());
        let close_code = match &closed {
            Some(quinn::ConnectionError::ApplicationClosed(closed)) => Some(closed.error_code),
            _ => None,
        };
        let deliberate = close_code == Some(close::MOVED) || close_code == Some(close::FORGOTTEN);
        if deliberate {
            self.stop_wanting_audio();
        }
        let network = !deliberate
            && (discontinuity
                || retryable(error.as_ref())
                || closed.as_ref().is_some_and(|e| retryable(e)));
        self.lost(&error, network);
        if close_code == Some(close::FORGOTTEN) {
            self.recovery = Recovery::PairAgain;
        }
        if close_code == Some(close::MOVED)
            && let Ok(observer) = connect(&self.identity).await
        {
            self.connected = Some(observer);
            self.error = None;
        }
    }

    async fn tick(&mut self, cancel: &tokio::sync::Notify) {
        self.check_audio_devices().await;
        if self
            .retry
            .is_some_and(|at| at <= tokio::time::Instant::now())
        {
            self.reconnect(cancel).await;
        }
    }

    async fn check_audio_devices(&mut self) {
        let Some(c) = self.connected.as_mut() else {
            return;
        };
        let Some(audio) = c.media.audio.as_ref().filter(|audio| audio.fault()) else {
            return;
        };
        let faults = audio.info().error;
        if faults == 0 {
            return;
        }
        let (input, output) = (faults & 1 != 0, faults & 2 != 0);
        if input && self.desired.input != 0 {
            self.desired.mic = false;
        }
        if output && self.desired.output != 0 {
            self.desired.playback = false;
        }
        match c.apply(&self.desired).await {
            Ok(()) => {
                self.recovery = Recovery::Automatic;
                if input && self.desired.input != 0 {
                    c.direction_errors[0] = Some(Failure::RouteDisconnected);
                }
                if output && self.desired.output != 0 {
                    c.direction_errors[1] = Some(Failure::RouteDisconnected);
                }
            }
            Err(e) => {
                self.retry_auto_route = (input && self.desired.mic && self.desired.input == 0)
                    || (output && self.desired.playback && self.desired.output == 0);
                let retry_auto_route = self.retry_auto_route;
                self.lost(&e, retry_auto_route);
            }
        }
    }

    async fn reconnect(&mut self, cancel: &tokio::sync::Notify) {
        let attempt = async {
            let mut c = connect(&self.identity).await?;
            c.apply(&self.desired).await?;
            Ok::<_, transport::Error>(c)
        };
        let result = tokio::select! {
            biased;
            _ = cancel.notified() => Err(Failure::Cancelled.into()),
            result = attempt => result,
        };
        let e = match result {
            Ok(c) => return self.connected(c),
            Err(e) => e,
        };
        if cause::<Failure>(e.as_ref()) == Some(&Failure::Cancelled) {
            self.stop_wanting_audio();
            return;
        }
        self.error = Some(Failure::of(e.as_ref()));
        self.recovery = Recovery::after(e.as_ref(), self.retry_auto_route);
        if self.recovery != Recovery::Automatic {
            self.retry = None;
            return;
        }
        self.attempt = (self.attempt + 1).min(5);
        let mut random = [0u8; 2];
        let _ = getrandom::fill(&mut random);
        let backoff =
            (500u64 << self.attempt).min(15000) + u64::from(u16::from_ne_bytes(random)) % 250;
        self.retry = Some(tokio::time::Instant::now() + Duration::from_millis(backoff));
    }
}

async fn run(
    identity: Identity,
    mut requests: tokio::sync::mpsc::Receiver<Request>,
    cancel: Arc<tokio::sync::Notify>,
    cancelled: Arc<AtomicBool>,
) {
    let mut supervisor = Supervisor::new(identity);
    let mut health = tokio::time::interval(Duration::from_millis(500));
    loop {
        match supervisor.next(&mut requests, &cancel, &mut health).await {
            Event::Cancelled => supervisor.cancel(),
            Event::Request(None) => break,
            Event::Request(Some(request)) => {
                if matches!(request.command, Command::Set { .. })
                    && cancelled.load(Ordering::Acquire)
                {
                    supervisor.shut_down();
                } else {
                    supervisor.command(request.command, &cancel).await;
                }
                let _ = request.reply.send(supervisor.status());
            }
            Event::HandedOff(result) => supervisor.handed_off(result).await,
            Event::Connection(Ok(())) => {}
            Event::Connection(Err(error)) => supervisor.connection_failed(error).await,
            Event::Tick => supervisor.tick(&cancel).await,
        }
    }
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
        let identity = crate::create_identity().unwrap();
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
        let mut identity = crate::create_identity().unwrap();
        identity.desktop = identity.certificate.clone();
        identity.endpoint = "127.0.0.1:9".parse().unwrap();
        let engine = Engine::new(identity).unwrap();
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
            let status = engine.set(settings).unwrap();
            assert!(!status.microphone.running && !status.listening.running);
            assert!(status.error.is_some());
            if reconnect {
                assert_eq!(status.state, SessionState::Reconnecting);
                assert!(status.reconnect_allowed);
            }
            let status = engine.stop().unwrap();
            assert!(!status.desired.mic && !status.desired.playback);
            assert_eq!(status.state, SessionState::Idle);
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
    fn only_the_computer_forgetting_the_phone_requires_pairing_again() {
        let closed = |code| {
            quinn::ReadError::ConnectionLost(quinn::ConnectionError::ApplicationClosed(
                quinn::ApplicationClose {
                    error_code: quinn::VarInt::from_u32(code),
                    reason: b"".as_slice().into(),
                },
            ))
        };
        assert_eq!(
            Recovery::after(&Failure::Revoked, false),
            Recovery::PairAgain
        );
        assert_eq!(Recovery::after(&closed(0x21), true), Recovery::PairAgain);
        assert_eq!(Recovery::after(&closed(0), false), Recovery::Automatic);
        for code in [1, 0x20, 0x22] {
            assert_eq!(Recovery::after(&closed(code), false), Recovery::Manual);
        }
        let refused = Failure::Refused;
        assert_eq!(Recovery::after(&refused, false), Recovery::Manual);
        assert_eq!(
            Recovery::after(&quinn::ConnectionError::TimedOut, false),
            Recovery::Automatic
        );
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
