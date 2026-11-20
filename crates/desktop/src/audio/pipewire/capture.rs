use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use litz_core::protocol::Route;

use crate::audio::{CAPACITY, Captured};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Capture {
    // Listeners must be dropped before the proxies they were registered on.
    _registry_listener: pw::registry::Listener,
    _metadata: Vec<(pw::metadata::MetadataListener, pw::metadata::Metadata)>,
    _listener: pw::stream::StreamListener<rtrb::Producer<Captured>>,
    _registry: pw::registry::RegistryRc,
    pub stream: pw::stream::StreamRc,
    pub samples: Option<rtrb::Consumer<Captured>>,
    pub fault: Arc<AtomicBool>,
    pub resolved: tokio::sync::watch::Sender<Option<String>>,
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

#[derive(Default)]
struct Devices {
    physical: BTreeMap<u32, String>,
    sinks: BTreeMap<u32, String>,
    default: Option<String>,
    selected: Option<u32>,
}
impl Devices {
    /// The sink WirePlumber links a stream without a target to.
    fn followed(&self) -> Option<String> {
        self.default
            .clone()
            .filter(|name| self.sinks.values().any(|sink| sink == name))
    }
}
fn physical(props: &spa::utils::dict::DictRef) -> bool {
    props.get("media.class") == Some("Audio/Sink")
        && props.get("device.id").is_some()
        && props.get("node.virtual") != Some("true")
        && props
            .get("node.name")
            .is_some_and(|name| !name.starts_with("litz."))
}
fn roundtrip(core: &pw::core::CoreRc, mainloop: &pw::main_loop::MainLoopRc) -> Result<()> {
    let seq = core.sync(0)?;
    let complete = Rc::new(std::cell::Cell::new(false));
    let done_complete = complete.clone();
    let _done = core
        .add_listener_local()
        .done(move |id, done| {
            if id == pw::core::PW_ID_CORE && done == seq {
                done_complete.set(true);
            }
        })
        .register();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    // Do not nest MainLoop::run/quit inside a control callback: quit would also
    // stop the agent's enclosing loop. Only dispatch until this sync completes.
    while !complete.get() {
        if std::time::Instant::now() >= deadline {
            return Err("PipeWire inventory timeout".into());
        }
        if mainloop.loop_().iterate(pw::loop_::Timeout::Finite(
            std::time::Duration::from_millis(20),
        )) < 0
        {
            return Err("PipeWire inventory dispatch failed".into());
        }
    }
    Ok(())
}
pub fn inventory(
    core: &pw::core::CoreRc,
    mainloop: &pw::main_loop::MainLoopRc,
) -> Result<Vec<litz_core::protocol::Endpoint>> {
    let registry = core.get_registry()?;
    let entries = Rc::new(RefCell::new(BTreeMap::new()));
    let added = entries.clone();
    let removed = entries.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == pw::types::ObjectType::Node
                && let Some(props) = global.props
                && physical(props)
                && let Some(name) = props.get("node.name")
            {
                let label = props.get("node.description").unwrap_or(name);
                if name.len() <= 256 && label.len() <= 256 && added.borrow().len() < 32 {
                    added.borrow_mut().insert(
                        global.id,
                        litz_core::protocol::Endpoint {
                            name: name.to_owned(),
                            label: label.to_owned(),
                        },
                    );
                }
            }
        })
        .global_remove(move |id| {
            removed.borrow_mut().remove(&id);
        })
        .register();
    roundtrip(core, mainloop)?;
    Ok(entries.borrow().values().cloned().collect())
}
/// Keeps `changed` marked whenever an output appears or goes away.
pub struct Outputs {
    _listener: pw::registry::Listener,
    _registry: pw::registry::RegistryRc,
}
pub fn watch_outputs(
    core: &pw::core::CoreRc,
) -> Result<(Outputs, tokio::sync::watch::Receiver<()>)> {
    let registry = core.get_registry_rc()?;
    let (changed, outputs) = tokio::sync::watch::channel(());
    let removed = changed.clone();
    let known = Rc::new(RefCell::new(std::collections::BTreeSet::new()));
    let gone = known.clone();
    let listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == pw::types::ObjectType::Node
                && global.props.is_some_and(physical)
                && known.borrow_mut().insert(global.id)
            {
                changed.send_replace(());
            }
        })
        .global_remove(move |id| {
            if gone.borrow_mut().remove(&id) {
                removed.send_replace(());
            }
        })
        .register();
    Ok((
        Outputs {
            _listener: listener,
            _registry: registry,
        },
        outputs,
    ))
}
pub fn open(
    core: &pw::core::CoreRc,
    mainloop: &pw::main_loop::MainLoopRc,
    route: &Route,
    wake: litz_core::wake::Wake,
) -> Result<Capture> {
    let registry = core.get_registry_rc()?;
    let devices = Rc::new(RefCell::new(Devices::default()));
    let metadata = Rc::new(RefCell::new(Vec::new()));
    let (resolved, _) = tokio::sync::watch::channel(None);
    let publish = {
        let resolved = resolved.clone();
        move |devices: &Devices| {
            let followed = devices.followed();
            resolved.send_if_modified(|current| {
                let changed = *current != followed;
                *current = followed;
                changed
            });
        }
    };
    let follow = matches!(route, Route::Mirror { name: None });
    let reg_devices = devices.clone();
    let reg_metadata = metadata.clone();
    let reg_publish = publish.clone();
    let core_clone = core.clone();
    let remove_devices = devices.clone();
    let remove_publish = publish.clone();
    let fault = Arc::new(AtomicBool::new(false));
    let remove_fault = fault.clone();
    let remove_wake = wake.clone();
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else {
                return;
            };
            if global.type_ == pw::types::ObjectType::Node
                && let Some(name) = props.get("node.name")
            {
                let mut d = reg_devices.borrow_mut();
                if physical(props) && d.physical.len() < 256 {
                    d.physical.insert(global.id, name.to_owned());
                }
                if props.get("media.class") == Some("Audio/Sink") && d.sinks.len() < 256 {
                    d.sinks.insert(global.id, name.to_owned());
                    if follow {
                        reg_publish(&d);
                    }
                }
            }
            if follow
                && global.type_ == pw::types::ObjectType::Metadata
                && props.get("metadata.name") == Some("default")
            {
                if !reg_metadata.borrow().is_empty() {
                    return;
                }
                let Ok(reg) = core_clone.get_registry() else {
                    return;
                };
                let Ok(meta) = reg.bind::<pw::metadata::Metadata, _>(global) else {
                    return;
                };
                let devices = reg_devices.clone();
                let publish = reg_publish.clone();
                let listener = meta
                    .add_listener_local()
                    .property(move |subject, key, _, value| {
                        if subject == 0 && key == Some("default.audio.sink") {
                            #[derive(serde::Deserialize)]
                            struct DefaultNode {
                                name: String,
                            }
                            let mut d = devices.borrow_mut();
                            d.default = value
                                .and_then(|v| serde_json::from_str::<DefaultNode>(v).ok())
                                .map(|v| v.name);
                            publish(&d);
                        }
                        0
                    })
                    .register();
                reg_metadata.borrow_mut().push((listener, meta));
            }
        })
        .global_remove(move |id| {
            let mut d = remove_devices.borrow_mut();
            d.physical.remove(&id);
            if d.sinks.remove(&id).is_some() && follow {
                remove_publish(&d);
            }
            if d.selected == Some(id) {
                remove_fault.store(true, Ordering::Release);
                remove_wake.signal();
            }
        })
        .register();
    roundtrip(core, mainloop)?;
    roundtrip(core, mainloop)?;
    let mut props = properties! {
        "node.name" => "litz.capture", "media.type" => "Audio", "media.category" => "Capture",
        "media.role" => "Music", "audio.channels" => "2", "audio.position" => "[ FL FR ]",
        "node.latency" => "512/48000", "node.pause-on-idle" => "true",
    };
    match route {
        Route::PhoneOnly => {
            props.insert("node.name", "litz.speaker");
            props.insert("node.description", "Litz Phone Speaker");
            props.insert("media.class", "Audio/Sink");
            props.insert("media.category", "Playback");
            props.insert("node.virtual", "true");
            props.insert("node.autoconnect", "false");
        }
        // WirePlumber links a stream without a target to the default sink,
        // moves it when the default changes, and with node.linger waits for
        // a sink instead of failing the stream while there is none.
        Route::Mirror { name: None } => {
            props.insert("stream.capture.sink", "true");
            props.insert("node.autoconnect", "true");
            props.insert("node.linger", "true");
        }
        Route::Mirror { name: Some(name) } => {
            let mut d = devices.borrow_mut();
            let id = d
                .physical
                .iter()
                .find(|(_, n)| *n == name)
                .map(|(id, _)| *id)
                .ok_or("route conflict: target is absent or not a physical sink")?;
            d.selected = Some(id);
            resolved.send_replace(Some(name.clone()));
            props.insert("target.object", name.as_str());
            props.insert("stream.capture.sink", "true");
            props.insert("node.autoconnect", "true");
            props.insert("node.dont-reconnect", "true");
            props.insert("node.dont-fallback", "true");
        }
    }
    let stream = pw::stream::StreamRc::new(core.clone(), "Litz Playback Capture", props)?;
    let (producer, consumer) = rtrb::RingBuffer::new(CAPACITY);
    let format_loop = mainloop.clone();
    let ready = Arc::new(AtomicBool::new(false));
    let process_ready = ready.clone();
    let listener = stream
        .add_local_listener_with_user_data(producer)
        .param_changed(move |_, _, id, param| {
            if id == spa::param::ParamType::Format.as_raw() {
                ready.store(false, Ordering::Release);
                let Some(pod) = param else {
                    return;
                };
                let mut info = AudioInfoRaw::new();
                if info.parse(pod).is_err()
                    || info.format() != AudioFormat::F32LE
                    || info.rate() != litz_core::pcm::RATE
                    || info.channels() != 2
                {
                    format_loop.quit();
                } else {
                    ready.store(true, Ordering::Release);
                }
            }
        })
        .process(move |stream, producer| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if !process_ready.load(Ordering::Acquire) {
                return;
            }
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            if !matches!(data.chunk().stride(), 0 | 8) {
                return;
            }
            let Some(bytes) = data.data() else {
                return;
            };
            let start = offset.min(bytes.len());
            let end = start.saturating_add(size).min(bytes.len());
            let at = std::time::Instant::now();
            for frame in bytes[start..end].as_chunks::<8>().0 {
                let left = f32::from_ne_bytes(frame[..4].try_into().unwrap());
                let right = f32::from_ne_bytes(frame[4..].try_into().unwrap());
                if producer.push(Captured::new([left, right], at)).is_err() {
                    break;
                }
            }
            wake.signal();
        })
        .register()?;
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(litz_core::pcm::RATE);
    info.set_channels(2);
    let mut positions = [0; spa::param::audio::MAX_CHANNELS];
    positions[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    positions[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(positions);
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )?
    .0
    .into_inner();
    let pod = spa::pod::Pod::from_bytes(&bytes).ok_or("format pod")?;
    let mut flags = pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS
        | pw::stream::StreamFlags::INACTIVE;
    if !matches!(route, Route::PhoneOnly) {
        flags |= pw::stream::StreamFlags::AUTOCONNECT;
    }
    stream.connect(spa::utils::Direction::Input, None, flags, &mut [pod])?;
    let metadata = std::mem::take(&mut *metadata.borrow_mut());
    Ok(Capture {
        _registry: registry,
        _registry_listener: registry_listener,
        _metadata: metadata,
        _listener: listener,
        stream,
        samples: Some(consumer),
        fault,
        resolved,
    })
}
