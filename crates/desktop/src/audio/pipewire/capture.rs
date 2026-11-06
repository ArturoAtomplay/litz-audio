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

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const CAPACITY: usize = 2048;
#[derive(Clone, Copy)]
pub struct Captured {
    pub samples: [f32; 2],
    pub at: std::time::Instant,
}

// Callback producer, engine consumer. Capacity accepts a full 2048-frame graph
// quantum; frames older than 40ms and overflow stop this direction, never catch up.
pub struct Capture {
    // Listeners must be dropped before the proxies they were registered on.
    _registry_listener: pw::registry::Listener,
    _metadata: Vec<(pw::metadata::MetadataListener, pw::metadata::Metadata)>,
    _listener: pw::stream::StreamListener<rtrb::Producer<Captured>>,
    _registry: pw::registry::RegistryRc,
    pub stream: pw::stream::StreamRc,
    pub samples: Option<rtrb::Consumer<Captured>>,
    pub fault: Arc<AtomicBool>,
    pub resolved: Option<String>,
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.stream.disconnect();
    }
}

#[derive(Default)]
struct Devices {
    physical: BTreeMap<u32, String>,
    default: Option<String>,
    selected: Option<u32>,
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
pub fn open(
    core: &pw::core::CoreRc,
    mainloop: &pw::main_loop::MainLoopRc,
    route: &Route,
    wake: litz_core::wake::Wake,
) -> Result<Capture> {
    let registry = core.get_registry_rc()?;
    let devices = Rc::new(RefCell::new(Devices::default()));
    let metadata = Rc::new(RefCell::new(Vec::new()));
    let reg_devices = devices.clone();
    let reg_metadata = metadata.clone();
    let core_clone = core.clone();
    let remove_devices = devices.clone();
    let fault = Arc::new(AtomicBool::new(false));
    let default_fault = fault.clone();
    let remove_fault = fault.clone();
    let default_wake = wake.clone();
    let remove_wake = wake.clone();
    let follow = matches!(route, Route::Mirror { name: None });
    let registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else {
                return;
            };
            if global.type_ == pw::types::ObjectType::Node
                && physical(props)
                && let Some(name) = props.get("node.name")
            {
                let mut d = reg_devices.borrow_mut();
                if d.physical.len() < 256 {
                    d.physical.insert(global.id, name.to_owned());
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
                let fault = default_fault.clone();
                let wake = default_wake.clone();
                let listener = meta
                    .add_listener_local()
                    .property(move |subject, key, _, value| {
                        if subject == 0 && key == Some("default.audio.sink") {
                            #[derive(serde::Deserialize)]
                            struct DefaultNode {
                                name: String,
                            }
                            let name = value
                                .and_then(|v| serde_json::from_str::<DefaultNode>(v).ok())
                                .map(|v| v.name);
                            let mut d = devices.borrow_mut();
                            if d.default != name && d.selected.is_some() {
                                fault.store(true, Ordering::Release);
                                wake.signal();
                            }
                            d.default = name;
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
            if d.selected == Some(id) {
                remove_fault.store(true, Ordering::Release);
                remove_wake.signal();
            }
        })
        .register();
    roundtrip(core, mainloop)?;
    roundtrip(core, mainloop)?;
    let target = match route {
        Route::PhoneOnly => None,
        Route::Mirror { name } => {
            let d = devices.borrow();
            let name = name
                .as_ref()
                .or(d.default.as_ref())
                .ok_or("no physical default sink")?;
            let id = d
                .physical
                .iter()
                .find(|(_, n)| *n == name)
                .map(|(id, _)| *id)
                .ok_or("route conflict: target is absent or not a physical sink")?;
            Some((id, name.clone()))
        }
    };
    devices.borrow_mut().selected = target.as_ref().map(|(id, _)| *id);
    let mut props = properties! {
        "node.name" => "litz.capture", "media.type" => "Audio", "media.category" => "Capture",
        "media.role" => "Music", "audio.channels" => "2", "audio.position" => "[ FL FR ]",
        "node.latency" => "256/48000", "node.dont-reconnect" => "true",
        "node.dont-fallback" => "true", "node.pause-on-idle" => "true",
    };
    if let Some((_, name)) = &target {
        props.insert("target.object", name.as_str());
        props.insert("stream.capture.sink", "true");
        props.insert("node.autoconnect", "true");
    } else {
        props.insert("node.name", "litz.speaker");
        props.insert("node.description", "Litz Phone Speaker");
        props.insert("media.class", "Audio/Sink");
        props.insert("media.category", "Playback");
        props.insert("node.virtual", "true");
        props.insert("node.autoconnect", "false");
    }
    let stream = pw::stream::StreamRc::new(core.clone(), "Litz Playback Capture", props)?;
    let (producer, consumer) = rtrb::RingBuffer::new(CAPACITY);
    let process_fault = fault.clone();
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
                if producer
                    .push(Captured {
                        samples: [
                            if left.is_finite() { left } else { 0.0 },
                            if right.is_finite() { right } else { 0.0 },
                        ],
                        at,
                    })
                    .is_err()
                {
                    process_fault.store(true, Ordering::Release);
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
    if target.is_some() {
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
        resolved: target.map(|(_, name)| name),
    })
}
