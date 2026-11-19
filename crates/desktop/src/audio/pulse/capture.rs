use super::{MIC_SINK, Result, SPEAKER, Server};
use crate::audio::{CAPACITY, Captured};
use libpulse_binding as pulse;
use litz_core::protocol::{Endpoint, Route};
use litz_core::wake::Wake;
use pulse::callbacks::ListResult;
use pulse::def::BufferAttr;
use pulse::proplist::properties;
use pulse::sample::{Format, Spec};
use pulse::stream::{FlagSet as StreamFlags, PeekResult, State as StreamState, Stream};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct Sink {
    name: String,
    label: String,
    monitor: String,
}

/// Outputs a person can listen to: every sink except Litz's own and
/// software sinks such as null, combine or filter sinks.
fn sinks(server: &mut Server) -> Result<Vec<Sink>> {
    server.wait(|context, answer| {
        let mut found = Vec::new();
        context
            .introspect()
            .get_sink_info_list(move |item| match item {
                ListResult::Item(sink) => {
                    let class = sink.proplist.get_str(properties::DEVICE_CLASS);
                    if let (Some(name), Some(monitor)) = (&sink.name, &sink.monitor_source_name)
                        && !name.starts_with("litz.")
                        && !matches!(class.as_deref(), Some("abstract" | "filter"))
                        && found.len() < 32
                    {
                        let label = sink.description.as_deref().unwrap_or(name);
                        if name.len() <= 256 && label.len() <= 256 {
                            found.push(Sink {
                                name: name.to_string(),
                                label: label.to_owned(),
                                monitor: monitor.to_string(),
                            });
                        }
                    }
                }
                _ => *answer.borrow_mut() = Some(std::mem::take(&mut found)),
            });
    })
}

pub fn outputs(server: &mut Server) -> Result<Vec<Endpoint>> {
    Ok(sinks(server)?
        .into_iter()
        .map(|sink| Endpoint {
            name: sink.name,
            label: sink.label,
        })
        .collect())
}

pub struct Capture {
    stream: Rc<RefCell<Stream>>,
    module: Option<u32>,
    follows: bool,
    /// Set while the default output is Litz's own microphone sink, which
    /// would send the phone's voice back to its speaker.
    muted: Rc<Cell<bool>>,
    failed: Rc<Cell<bool>>,
    pub samples: Option<rtrb::Consumer<Captured>>,
    pub fault: Arc<AtomicBool>,
    pub resolved: tokio::sync::watch::Sender<Option<String>>,
}

impl Capture {
    pub fn open(server: &mut Server, route: &Route, wake: Wake) -> Result<Self> {
        let (resolved, _) = tokio::sync::watch::channel(None);
        let mut module = None;
        let device = match route {
            Route::PhoneOnly => {
                module = Some(server.load(
                    "module-null-sink",
                    &format!(
                        "sink_name={SPEAKER} format=float32le rate={} channels=2 \
                         sink_properties='device.description=\"Litz Phone Speaker\"'",
                        litz_core::pcm::RATE
                    ),
                )?);
                format!("{SPEAKER}.monitor")
            }
            Route::Mirror { name: None } => "@DEFAULT_MONITOR@".into(),
            Route::Mirror { name: Some(name) } => {
                let sink = sinks(server)?
                    .into_iter()
                    .find(|sink| sink.name == *name)
                    .ok_or("route conflict: target is absent or not a physical sink")?;
                resolved.send_replace(Some(sink.name));
                sink.monitor
            }
        };
        let follows = matches!(route, Route::Mirror { name: None });
        let spec = Spec {
            format: Format::F32le,
            rate: litz_core::pcm::RATE,
            channels: 2,
        };
        let Some(stream) = Stream::new(&mut server.context, "Litz Playback Capture", &spec, None)
        else {
            if let Some(module) = module {
                server.unload(module);
            }
            return Err("PulseAudio capture stream".into());
        };
        let mut capture = Self {
            stream: Rc::new(RefCell::new(stream)),
            module,
            follows,
            muted: Rc::default(),
            failed: Rc::default(),
            samples: None,
            fault: Arc::default(),
            resolved,
        };
        let opened = capture.connect(server, &device, wake).and_then(|()| {
            if follows {
                capture.follow(server.default_sink()?);
            }
            Ok(())
        });
        if let Err(error) = opened {
            capture.close(server);
            server.finish();
            return Err(error);
        }
        Ok(capture)
    }

    fn connect(&mut self, server: &mut Server, device: &str, wake: Wake) -> Result<()> {
        let (mut producer, consumer) = rtrb::RingBuffer::new(CAPACITY);
        self.samples = Some(consumer);
        let reader = Rc::downgrade(&self.stream);
        let muted = self.muted.clone();
        let fault_wake = wake.clone();
        let mut stream = self.stream.borrow_mut();
        stream.set_read_callback(Some(Box::new(move |_| {
            let Some(stream) = reader.upgrade() else {
                return;
            };
            let mut stream = stream.borrow_mut();
            let at = std::time::Instant::now();
            while let Ok(peeked) = stream.peek() {
                match peeked {
                    PeekResult::Empty => break,
                    PeekResult::Hole(_) => {}
                    PeekResult::Data(bytes) if !muted.get() => {
                        for frame in bytes.as_chunks::<8>().0 {
                            let left = f32::from_ne_bytes(frame[..4].try_into().unwrap());
                            let right = f32::from_ne_bytes(frame[4..].try_into().unwrap());
                            if producer.push(Captured::new([left, right], at)).is_err() {
                                break;
                            }
                        }
                    }
                    PeekResult::Data(_) => {}
                }
                if stream.discard().is_err() {
                    break;
                }
            }
            wake.signal();
        })));
        let watched = Rc::downgrade(&self.stream);
        let failed = self.failed.clone();
        let fault = self.fault.clone();
        let follows = self.follows;
        stream.set_state_callback(Some(Box::new(move || {
            let Some(stream) = watched.upgrade() else {
                return;
            };
            let Ok(stream) = stream.try_borrow() else {
                return;
            };
            if matches!(
                stream.get_state(),
                StreamState::Failed | StreamState::Terminated
            ) {
                if follows {
                    failed.set(true);
                } else {
                    fault.store(true, Ordering::Release);
                    fault_wake.signal();
                }
            }
        })));
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 512 * 8,
        };
        let mut flags = StreamFlags::START_CORKED | StreamFlags::ADJUST_LATENCY;
        if !self.follows {
            flags |= StreamFlags::DONT_MOVE;
        }
        stream.connect_record(Some(device), Some(&attr), flags)?;
        drop(stream);
        let ready = self.stream.clone();
        server.settle(|| ready.borrow().get_state() == StreamState::Ready)
    }

    /// Publishes the default output the server has moved this capture to.
    pub fn follow(&self, default: Option<String>) {
        if !self.follows {
            return;
        }
        let own = default.as_deref() == Some(MIC_SINK);
        self.muted.set(own);
        let default = default.filter(|_| !own);
        self.resolved.send_if_modified(|current| {
            let changed = *current != default;
            *current = default;
            changed
        });
    }

    pub fn is_speaker(&self) -> bool {
        self.module.is_some()
    }

    pub fn lost(&self) -> bool {
        self.failed.get()
    }

    pub fn set_active(&mut self, server: &mut Server, active: bool) -> Result<()> {
        let stream = self.stream.clone();
        let applied = server.wait(move |_, answer| {
            let done = move |success| *answer.borrow_mut() = Some(success);
            let mut stream = stream.borrow_mut();
            if active {
                stream.uncork(Some(Box::new(done)));
            } else {
                stream.cork(Some(Box::new(done)));
            }
        })?;
        if applied {
            Ok(())
        } else {
            Err("PulseAudio refused to change the capture".into())
        }
    }

    pub fn close(self, server: &mut Server) {
        let mut stream = self.stream.borrow_mut();
        stream.set_read_callback(None);
        stream.set_state_callback(None);
        let _ = stream.disconnect();
        drop(stream);
        if let Some(module) = self.module {
            server.unload(module);
        }
    }
}
