use crate::audio::voice::Worker;
use crate::pairing::Hub;
use crate::server::{Ports, apply_route, capture_active, desktop_routes, reopen_capture};
use litz_core::{
    media::{
        codec::Encoder,
        receiver::{CHUNK, Receiver},
    },
    pcm::PcmProducer,
    protocol::{
        Code, Config, DesktopRoutes, FRAME, MAX_DATAGRAM, Message, Record, Route, Stream,
        VoiceRuntime, VoiceSettings,
    },
    session::Session,
    transport::{self, Control, MediaSender, Result},
    wake::WakeListener,
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::task::{JoinError, JoinHandle};

pub enum Exit {
    Released(u32),
    Unpaired,
}

type Task<T> = JoinHandle<std::result::Result<T, String>>;

struct MicStart {
    request_id: u32,
    config: Config,
    worker: Task<Worker>,
}

enum VoiceChange {
    Preparing {
        request_id: u32,
        settings: VoiceSettings,
        expected: u64,
        task: Task<Option<Worker>>,
    },
    Committing {
        request_id: u32,
        settings: VoiceSettings,
        expected: u64,
        next: Option<Worker>,
        task: Task<()>,
    },
}

enum VoiceStep {
    Prepared(std::result::Result<std::result::Result<Option<Worker>, String>, JoinError>),
    Persisted(std::result::Result<std::result::Result<(), String>, JoinError>),
}

async fn next_step(change: &mut VoiceChange) -> VoiceStep {
    match change {
        VoiceChange::Preparing { task, .. } => VoiceStep::Prepared(task.await),
        VoiceChange::Committing { task, .. } => VoiceStep::Persisted(task.await),
    }
}

/// Workers join their thread on drop; never do that on the runtime.
fn retire(worker: Option<Worker>) {
    if let Some(worker) = worker {
        tokio::task::spawn_blocking(move || drop(worker));
    }
}

struct Streaming<'a> {
    c: &'a quinn::Connection,
    control: &'a mut Control,
    ports: &'a mut Ports,
    hub: &'a Arc<Hub>,
    session: Session,
    voice: Option<Worker>,
    receiver: Option<Receiver>,
    encoder: Option<Encoder>,
    mic_pending: [f32; FRAME],
    mic_offset: usize,
    mic_len: usize,
    captured: [f32; FRAME * 2],
    captured_frames: usize,
    mic_start: Option<MicStart>,
    voice_change: Option<VoiceChange>,
    advertised: DesktopRoutes,
    default_retry: bool,
    packet: [u8; MAX_DATAGRAM],
    sender: MediaSender,
    reduced_at: Instant,
    origin: Instant,
}

pub async fn run(
    c: &quinn::Connection,
    control: &mut Control,
    ports: &mut Ports,
    hub: &Arc<Hub>,
) -> Result<Exit> {
    let advertised = desktop_routes(ports).await?;
    let mut stream = Box::new(Streaming {
        c,
        session: Session::new(ports.revision),
        default_retry: ports.resolved.is_none(),
        control,
        ports,
        hub,
        voice: None,
        receiver: None,
        encoder: None,
        mic_pending: [0.0; FRAME],
        mic_offset: 0,
        mic_len: 0,
        captured: [0.0; FRAME * 2],
        captured_frames: 0,
        mic_start: None,
        voice_change: None,
        advertised,
        packet: [0; MAX_DATAGRAM],
        sender: MediaSender::default(),
        reduced_at: Instant::now(),
        origin: Instant::now(),
    });
    let result = stream.serve().await;
    stream.ports.reset.store(true, Ordering::Release);
    if let Some(worker) = stream.voice.take() {
        tokio::task::spawn_blocking(move || drop(worker)).await?;
    }
    result
}

impl Streaming<'_> {
    async fn serve(&mut self) -> Result<Exit> {
        let mut voice_watch = tokio::time::interval(Duration::from_secs(1));
        voice_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut route_watch = tokio::time::interval(Duration::from_secs(1));
        route_watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let wake = WakeListener::try_new(self.ports.wake.clone())?;
        self.ports.reset.store(true, Ordering::Release);
        while self.ports.demand.pop().is_ok() {}
        self.drain_capture();
        loop {
            tokio::select! {
                biased;
                prepared = async { (&mut self.mic_start.as_mut().unwrap().worker).await },
                    if self.mic_start.is_some() => self.mic_ready(prepared).await?,
                _ = voice_watch.tick() => self.watch_voice().await?,
                step = async { next_step(self.voice_change.as_mut().unwrap()).await },
                    if self.voice_change.is_some() => self.voice_step(step).await?,
                _ = route_watch.tick(), if self.follows_default() => self.watch_default().await?,
                request = self.control.read() => {
                    if let Some(exit) = self.request(request?).await? {
                        return Ok(exit);
                    }
                }
                datagram = self.c.read_datagram() => self.datagram(&datagram?)?,
                ready = wake.wait() => {
                    ready?;
                    self.audio_ready().await?;
                }
            }
        }
    }

    fn follows_default(&self) -> bool {
        matches!(self.ports.route, Some(Route::Mirror { name: None }))
    }

    fn status(&self) -> Message {
        Message::Status {
            revision: self.session.revision,
            running: self.session.running,
        }
    }

    fn voice_runtime(&self) -> Option<VoiceRuntime> {
        self.receiver.as_ref()?;
        self.voice.as_ref().map(Worker::runtime)
    }

    fn capabilities(&self) -> Message {
        Message::Capabilities {
            desktop: Some(self.advertised.clone()),
        }
    }

    async fn reply(&mut self, request_id: u32, message: Message) -> Result<()> {
        self.control.write(&Record::new(request_id, message)).await
    }

    async fn error(&mut self, request_id: u32, code: Code) -> Result<()> {
        self.reply(request_id, Message::Error { code }).await
    }

    fn reset_mic(&mut self) {
        self.mic_offset = 0;
        self.mic_len = 0;
    }

    fn drain_capture(&mut self) {
        if let Some(capture) = &mut self.ports.capture {
            while capture.pop().is_ok() {}
        }
    }

    async fn mic_ready(
        &mut self,
        prepared: std::result::Result<std::result::Result<Worker, String>, JoinError>,
    ) -> Result<()> {
        let MicStart {
            request_id, config, ..
        } = self.mic_start.take().ok_or("missing microphone start")?;
        let next = match prepared.map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(worker) => worker,
            Err(error) => {
                eprintln!("litzd: microphone voice preparation failed: {error}");
                return self.error(request_id, Code::Invalid).await;
            }
        };
        if self.encoder.is_none()
            && self.ports.capture.is_some()
            && self.ports.route.is_some()
            && capture_active(self.ports, true).await.is_err()
        {
            retire(Some(next));
            return self.error(request_id, Code::RouteConflict).await;
        }
        retire(self.voice.replace(next));
        self.receiver = Some(Receiver::try_new(config)?);
        self.reset_mic();
        self.ports.reset.store(true, Ordering::Release);
        self.session.start(config).map_err(|_| "start")?;
        self.reply(request_id, self.status()).await
    }

    async fn watch_voice(&mut self) -> Result<()> {
        let state = self.voice_runtime();
        if self.advertised.voice_runtime == state {
            return Ok(());
        }
        self.advertised.voice_runtime = state;
        self.advertised.voice = self.ports.voice;
        self.advertised.revision = self.ports.revision;
        self.advertised.effective = self.ports.route.clone();
        self.advertised.resolved = self.ports.resolved.clone();
        self.reply(0, self.capabilities()).await
    }

    async fn voice_step(&mut self, step: VoiceStep) -> Result<()> {
        match (self.voice_change.take(), step) {
            (
                Some(VoiceChange::Preparing {
                    request_id,
                    settings,
                    expected,
                    ..
                }),
                VoiceStep::Prepared(prepared),
            ) => {
                let prepared = prepared.map_err(|e| e.to_string()).and_then(|r| r);
                let next = match prepared {
                    Ok(next) if self.session.revision == expected => next,
                    Ok(next) => {
                        retire(next);
                        return self.error(request_id, Code::RevisionConflict).await;
                    }
                    Err(_) => return self.error(request_id, Code::Invalid).await,
                };
                let saved = crate::settings::Settings {
                    revision: self
                        .ports
                        .revision
                        .checked_add(1)
                        .ok_or("settings revision exhausted")?,
                    route: self.ports.route.clone(),
                    voice: settings,
                };
                let task = tokio::task::spawn_blocking(move || {
                    crate::settings::save(&saved).map_err(|e| e.to_string())
                });
                self.voice_change = Some(VoiceChange::Committing {
                    request_id,
                    settings,
                    expected,
                    next,
                    task,
                });
                Ok(())
            }
            (
                Some(VoiceChange::Committing {
                    request_id,
                    settings,
                    expected,
                    next,
                    ..
                }),
                VoiceStep::Persisted(persisted),
            ) => {
                if self.session.revision != expected {
                    retire(next);
                    return self.error(request_id, Code::RevisionConflict).await;
                }
                if !matches!(persisted, Ok(Ok(()))) {
                    retire(next);
                    return self.error(request_id, Code::Invalid).await;
                }
                if let Some(mut next) = next {
                    if let Some(old) = &self.voice {
                        next.transition_from(old);
                    }
                    retire(self.voice.replace(next));
                } else if let Some(current) = &mut self.voice {
                    current.configure(settings);
                }
                // Keep the pending PCM tail: dropping it splices the waveform.
                let revision = self
                    .ports
                    .revision
                    .checked_add(1)
                    .ok_or("settings revision exhausted")?;
                self.ports.voice = settings;
                self.ports.revision = revision;
                self.session.revision = revision;
                self.reply(request_id, self.status()).await
            }
            _ => Err("voice change out of order".into()),
        }
    }

    async fn watch_default(&mut self) -> Result<()> {
        if !self.default_retry && !self.ports.capture_fault.load(Ordering::Acquire) {
            return Ok(());
        }
        let active = self.encoder.is_some();
        if reopen_capture(self.ports, Route::Mirror { name: None }).await? {
            self.default_retry = false;
            self.captured_frames = 0;
            if active {
                capture_active(self.ports, true).await?;
            }
        } else {
            self.default_retry = true;
            self.ports.resolved = None;
        }
        let state = self.advertised.voice_runtime;
        self.advertised = desktop_routes(self.ports).await?;
        self.advertised.voice_runtime = state;
        self.reply(0, self.capabilities()).await
    }

    async fn request(&mut self, request: Record) -> Result<Option<Exit>> {
        let id = request.request_id;
        let message = match request.message {
            Message::Unpair {} => {
                let cert = transport::peer_certificate(self.c)?;
                self.hub.forget(&cert).await?;
                return Ok(Some(Exit::Unpaired));
            }
            Message::Release {} => return Ok(Some(Exit::Released(id))),
            Message::GetOwnership {} => self.hub.status(self.c),
            Message::GetState {} => self.status(),
            Message::Capabilities { .. } => {
                self.advertised = desktop_routes(self.ports).await?;
                self.advertised.voice_runtime = self.voice_runtime();
                self.capabilities()
            }
            Message::Start(config) => match self.start(id, config).await? {
                Some(message) => message,
                None => return Ok(None),
            },
            Message::Stop { stream } => self.stop(stream).await?,
            Message::ApplyVoice {
                expected_revision,
                voice,
            } => match self.apply_voice(id, expected_revision, voice) {
                Some(message) => message,
                None => return Ok(None),
            },
            Message::ApplySettings {
                expected_revision,
                route,
            } => self.apply_settings(expected_revision, route).await?,
            _ => Message::Error {
                code: Code::Unsupported,
            },
        };
        self.reply(id, message).await?;
        Ok(None)
    }

    /// Returns `None` when the reply is sent later, after the voice worker is ready.
    async fn start(&mut self, request_id: u32, config: Config) -> Result<Option<Message>> {
        let error = |code| Ok(Some(Message::Error { code }));
        if !config.valid() {
            return error(Code::Invalid);
        }
        if config.stream == Stream::Mic && self.mic_start.is_some() {
            return error(Code::Busy);
        }
        if let Err(code) = self.session.validate_start(config) {
            return error(code);
        }
        if config.stream == Stream::Mic {
            if !self.ports.mic_available.load(Ordering::Acquire) {
                return error(Code::RouteConflict);
            }
            let mut settings = self.ports.voice;
            if !crate::server::available_suppression().contains(&settings.suppression) {
                eprintln!("litzd: saved noise suppression is not installed; starting without it");
                settings.suppression = litz_core::protocol::Suppression::None;
            }
            self.mic_start = Some(MicStart {
                request_id,
                config,
                worker: tokio::task::spawn_blocking(move || Worker::try_new(settings)),
            });
            return Ok(None);
        }
        if self.ports.route.is_none() || self.ports.capture_fault.load(Ordering::Acquire) {
            return error(Code::RouteConflict);
        }
        let encoder = Encoder::try_new(config)?;
        self.drain_capture();
        self.ports.capture_fault.store(false, Ordering::Release);
        if capture_active(self.ports, true).await.is_err() {
            return error(Code::RouteConflict);
        }
        self.encoder = Some(encoder);
        self.captured_frames = 0;
        self.session.start(config).map_err(|_| "start")?;
        Ok(Some(self.status()))
    }

    async fn stop(&mut self, stream: Stream) -> Result<Message> {
        if stream == Stream::Mic && self.mic_start.is_some() {
            return Ok(Message::Error { code: Code::Busy });
        }
        self.session.stop(stream);
        if stream == Stream::Mic {
            retire(self.voice.take());
            self.receiver = None;
            self.reset_mic();
            if self.encoder.is_none() && self.ports.capture.is_some() {
                let _ = capture_active(self.ports, false).await;
            }
            self.ports.reset.store(true, Ordering::Release);
        } else {
            if self.ports.capture.is_some()
                && let Err(error) = capture_active(self.ports, false).await
            {
                eprintln!("litzd: capture deactivation failed: {error}");
            }
            self.encoder = None;
            self.captured_frames = 0;
            self.drain_capture();
        }
        Ok(self.status())
    }

    /// Returns `None` when the reply is sent later, after the change commits.
    fn apply_voice(
        &mut self,
        request_id: u32,
        expected: u64,
        settings: VoiceSettings,
    ) -> Option<Message> {
        let error = |code| Some(Message::Error { code });
        if expected != self.session.revision {
            return error(Code::RevisionConflict);
        }
        if self.voice_change.is_some() {
            return error(Code::Busy);
        }
        if !crate::server::available_suppression().contains(&settings.suppression) {
            return error(Code::Unsupported);
        }
        let current = self.voice.as_ref();
        let reuse = current.is_some_and(|voice| voice.can_configure(settings));
        let history = current.map(Worker::history).unwrap_or_default();
        let level = current.and_then(Worker::level);
        let task = tokio::task::spawn_blocking(move || {
            if reuse {
                Ok(None)
            } else {
                Worker::warm(settings, history, level).map(Some)
            }
        });
        self.voice_change = Some(VoiceChange::Preparing {
            request_id,
            settings,
            expected,
            task,
        });
        None
    }

    async fn apply_settings(&mut self, expected: u64, route: Route) -> Result<Message> {
        if expected != self.session.revision {
            return Ok(Message::Error {
                code: Code::RevisionConflict,
            });
        }
        if matches!(self.voice_change, Some(VoiceChange::Committing { .. })) {
            return Ok(Message::Error { code: Code::Busy });
        }
        let applied = self.session.running[1].is_none() && apply_route(self.ports, route).await?;
        self.session.revision = self.ports.revision;
        if !applied {
            return Ok(Message::Error {
                code: Code::RouteConflict,
            });
        }
        if self.receiver.is_some() {
            capture_active(self.ports, true).await?;
        }
        self.default_retry = false;
        self.captured_frames = 0;
        self.sender = MediaSender::default();
        Ok(self.status())
    }

    fn datagram(&mut self, datagram: &[u8]) -> Result<()> {
        let Some(receiver) = &mut self.receiver else {
            return Ok(());
        };
        let generation = receiver.generation();
        let _ = receiver.push(datagram, self.origin.elapsed().as_secs_f64());
        if receiver.generation() != generation {
            self.reset_mic();
            self.ports.reset.store(true, Ordering::Release);
            if let Some(voice) = &mut self.voice {
                voice.reset()?;
            }
        }
        Ok(())
    }

    async fn audio_ready(&mut self) -> Result<()> {
        if self.receiver.is_some() && !self.ports.mic_available.load(Ordering::Acquire) {
            return Err("Virtual microphone endpoint disconnected".into());
        }
        self.fill_demand().await?;
        self.send_capture().await
    }

    async fn fill_demand(&mut self) -> Result<()> {
        while let Ok(demand) = self.ports.demand.pop() {
            let Some(receiver) = &mut self.receiver else {
                continue;
            };
            let stale = demand.at.elapsed() > Duration::from_millis(40);
            if stale || demand.reset {
                receiver.clear();
                self.mic_offset = 0;
                self.mic_len = 0;
                if let Some(voice) = &mut self.voice {
                    voice.reset()?;
                }
            }
            if stale {
                self.ports.reset.store(true, Ordering::Release);
                continue;
            }
            let now = demand.at.saturating_duration_since(self.origin);
            tokio::time::timeout(
                Duration::from_millis(40),
                fill_mic(
                    &mut self.ports.mic,
                    receiver,
                    demand.frames,
                    now.as_secs_f64(),
                    &mut self.mic_pending,
                    (&mut self.mic_offset, &mut self.mic_len),
                    self.voice.as_mut(),
                ),
            )
            .await??;
        }
        Ok(())
    }

    async fn send_capture(&mut self) -> Result<()> {
        let mut failed = self.ports.capture_fault.swap(false, Ordering::AcqRel);
        if let Some(capture) = &mut self.ports.capture {
            for _ in 0..crate::audio::capture::CAPACITY {
                let Ok(frame) = capture.pop() else { break };
                if frame.at.elapsed() > Duration::from_millis(40) {
                    failed = true;
                }
                let Some(encoder) = &mut self.encoder else {
                    continue;
                };
                if failed {
                    continue;
                }
                let at = self.captured_frames * 2;
                self.captured[at..at + 2].copy_from_slice(&frame.samples);
                self.captured_frames += 1;
                if self.captured_frames < FRAME {
                    continue;
                }
                self.captured_frames = 0;
                let limit = self.c.max_datagram_size().unwrap_or(0);
                let size = encoder.encode(&self.captured, &mut self.packet, limit)?;
                let sent = self.sender.send(self.c, &self.packet[..size]).await?;
                if !sent && self.reduced_at.elapsed() > Duration::from_secs(1) {
                    encoder.reduce_bitrate()?;
                    self.reduced_at = Instant::now();
                } else if sent && self.reduced_at.elapsed() > Duration::from_secs(10) {
                    encoder.recover_bitrate()?;
                    self.reduced_at = Instant::now();
                }
            }
        }
        if !failed {
            return Ok(());
        }
        if self.follows_default() {
            self.default_retry = true;
            self.ports.resolved = None;
            self.captured_frames = 0;
            let _ = capture_active(self.ports, false).await;
            return Ok(());
        }
        self.ports.route = None;
        self.ports.resolved = None;
        if self.encoder.is_some() {
            if let Err(error) = capture_active(self.ports, false).await {
                eprintln!("litzd: capture deactivation failed: {error}");
            }
            self.session.stop(Stream::Playback);
            self.encoder = None;
            self.captured_frames = 0;
            self.error(0, Code::RouteConflict).await?;
        }
        Ok(())
    }
}

async fn fill_mic(
    producer: &mut PcmProducer,
    receiver: &mut Receiver,
    frames: usize,
    now: f64,
    pending: &mut [f32; FRAME],
    (offset, len): (&mut usize, &mut usize),
    mut voice: Option<&mut Worker>,
) -> Result<()> {
    // Voice processing works on whole codec frames, so it decodes a frame ahead.
    let block = if voice.is_some() { FRAME } else { CHUNK };
    receiver.set_render_quantum(frames.max(block));
    let mut remaining = frames.min(litz_core::pcm::CAPACITY);
    while remaining > 0 {
        if *offset == *len {
            let generation = receiver.generation();
            for chunk in pending[..block].as_chunks_mut::<CHUNK>().0 {
                receiver.render(now, chunk)?;
            }
            if let Some(voice) = &mut voice {
                if receiver.generation() != generation {
                    voice.reset()?;
                }
                voice.process(pending).await?;
            }
            *offset = 0;
            *len = block;
        }
        let take = remaining.min(*len - *offset).min(producer.free_samples());
        if take == 0 {
            break;
        }
        producer.push(&pending[*offset..*offset + take]);
        *offset += take;
        remaining -= take;
    }
    Ok(())
}
