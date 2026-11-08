//! Real agent + private PipeWire, no hardware or global routing changes.
#![cfg(target_os = "linux")]
use litz_core::{
    protocol::{Code, Config, DeviceId, Message, Record, Route, Stream},
    transport::{self, Control},
};
use std::{
    fs,
    io::Write,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    children: Vec<Child>,
    daemon_input: Option<std::process::ChildStdin>,
    directory: std::path::PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.daemon_input = None;
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Only this test's exact files; never remove a user directory recursively.
        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| !t.is_dir()) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let state = self.directory.join("litz");
        for file in ["identity.json", "settings.json", "agent.lock"] {
            let _ = fs::remove_file(state.join(file));
        }
        let _ = fs::remove_dir(state);
        let _ = fs::remove_dir(&self.directory);
    }
}
async fn rpc(control: &mut Control, id: u32, message: Message) -> Message {
    control.write(&Record::new(id, message)).await.unwrap();
    let response = loop {
        let response = tokio::time::timeout(Duration::from_secs(5), control.read())
            .await
            .unwrap()
            .unwrap();
        if response.request_id == 0 && matches!(response.message, Message::Capabilities { .. }) {
            continue;
        }
        break response;
    };
    assert_eq!(response.request_id, id);
    response.message
}
async fn enroll(connection: &quinn::Connection, fixture: &mut Fixture) {
    let mut control = Control::new(connection.open_bi().await.unwrap());
    let id = [42; 16];
    let nonce = [5; 32];
    control
        .write(&Record::new(
            1,
            Message::PairRequest {
                id: DeviceId(id),
                name: "Test phone".into(),
                commitment: litz_core::session::commitment(&nonce),
            },
        ))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), control.read())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(reply.message, Message::PairNonce { .. }));
    control
        .write(&Record::new(1, Message::PairReveal { nonce }))
        .await
        .unwrap();
    // Without a session bus the daemon asks on stdin; allow it. The result
    // arrives only after the person approves, mirroring a phone.
    fixture
        .daemon_input
        .as_mut()
        .expect("daemon stdin")
        .write_all(b"y\n")
        .unwrap();
    fixture.daemon_input.as_mut().unwrap().flush().unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(10), control.read())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        reply.message,
        Message::PairResult {
            id,
            outcome: litz_core::protocol::PairOutcome::Accepted,
        } if id == DeviceId([42; 16])
    ));
}

fn capabilities() -> Message {
    Message::Capabilities { desktop: None }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires pipewire executable; run in nix develop"]
async fn live_routes_are_revision_checked_and_do_not_restart_the_agent() {
    let mut nonce = [0; 8];
    getrandom::fill(&mut nonce).unwrap();
    let directory =
        std::env::temp_dir().join(format!("litz-routes-{:016x}", u64::from_ne_bytes(nonce)));
    fs::create_dir(&directory).unwrap();
    let mut fixture = Fixture {
        children: vec![],
        daemon_input: None,
        directory: directory.clone(),
    };
    let config = directory.join("pipewire.conf");
    fs::write(&config, r#"
context.properties = { core.daemon = true core.name = pipewire-0 }
context.spa-libs = { support.* = support/libspa-support audio.convert.* = audioconvert/libspa-audioconvert }
context.modules = [
 { name = libpipewire-module-protocol-native }
 { name = libpipewire-module-client-node }
 { name = libpipewire-module-adapter }
 { name = libpipewire-module-metadata }
 { name = libpipewire-module-access args = { access.force = unrestricted } }
]
context.objects = [
 { factory = metadata args = { metadata.name = default } }
 { factory = adapter args = { factory.name = support.null-audio-sink node.name = test.output.two node.description = "Second physical output" media.class = Audio/Sink audio.position = [ FL FR ] device.id = 2 node.virtual = false } }
 { factory = adapter args = { factory.name = support.null-audio-sink node.name = test.output node.description = "Test physical output" media.class = Audio/Sink audio.position = [ FL FR ] device.id = 1 node.virtual = false } }
]
"#).unwrap();
    fixture.children.push(
        Command::new("pipewire")
            .args(["-c", config.to_str().unwrap()])
            .env("XDG_RUNTIME_DIR", &directory)
            .env("PIPEWIRE_RUNTIME_DIR", &directory)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !directory.join("pipewire-0").exists() {
        assert!(Instant::now() < deadline, "private PipeWire did not start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let binary = env!("CARGO_BIN_EXE_litzd");
    let desktop = rcgen::generate_simple_self_signed(vec!["litz.local".into()]).unwrap();
    let phone = rcgen::generate_simple_self_signed(vec!["litz.local".into()]).unwrap();
    let cert = desktop.cert.der().to_vec();
    let state = directory.join("litz");
    fs::create_dir(&state).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let identity = serde_json::json!({"registry_version":1,
        "certificate":cert, "private_key":desktop.signing_key.serialize_der(),
        "bind":address, "trust":{"phones":[]}});
    fs::write(state.join("identity.json"), identity.to_string()).unwrap();
    fs::set_permissions(
        state.join("identity.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fixture.children.push(
        Command::new(binary)
            .env("LITZ_DISABLE_DISCOVERY", "1")
            // Keep the private fixture from advertising on the user's LAN.
            .env("PATH", &directory)
            .env("XDG_STATE_HOME", &directory)
            .env("XDG_RUNTIME_DIR", &directory)
            .env("PIPEWIRE_RUNTIME_DIR", &directory)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    fixture.daemon_input = fixture.children[1].stdin.take();
    let mut endpoint = transport::client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(
        transport::client(
            cert.clone(),
            phone.cert.der().to_vec(),
            phone.signing_key.serialize_der(),
        )
        .unwrap(),
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    let pairing_connection = tokio::time::timeout(
        Duration::from_secs(5),
        endpoint.connect(address, "litz.local").unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    enroll(&pairing_connection, &mut fixture).await;
    let cli_directory = fixture.directory.clone();
    let litzd = move |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_litzd"))
            .args(args)
            .env("XDG_STATE_HOME", &cli_directory)
            .env("XDG_RUNTIME_DIR", &cli_directory)
            .output()
            .unwrap()
    };
    let devices = litzd(&["devices"]);
    assert!(String::from_utf8_lossy(&devices.stdout).contains("Test phone"));
    let connection = endpoint
        .connect(address, "litz.local")
        .unwrap()
        .await
        .unwrap();
    let mut control = Control::new(connection.open_bi().await.unwrap());
    assert!(matches!(
        rpc(&mut control, 1, Message::GetState {}).await,
        Message::Status { .. }
    ));
    assert!(matches!(
        rpc(
            &mut control,
            2,
            Message::Acquire {
                expected_generation: None
            }
        )
        .await,
        Message::Ownership { owned: true, .. }
    ));
    let mic = Config {
        stream: Stream::Mic,
        epoch: 1,
    };
    assert!(matches!(
        rpc(&mut control, 2, Message::Start(mic)).await,
        Message::Status {
            running: [Some(_), None],
            ..
        }
    ));
    let Message::Capabilities {
        desktop: Some(inventory),
        ..
    } = rpc(&mut control, 3, capabilities()).await
    else {
        panic!("missing inventory")
    };
    assert!(
        inventory
            .endpoints
            .iter()
            .any(|e| e.name == "test.output" && e.label == "Test physical output")
    );
    assert_eq!(inventory.effective, Some(Route::Mirror { name: None }));
    assert_eq!(inventory.resolved, None);
    let route = Route::Mirror {
        name: Some("test.output".into()),
    };
    assert!(
        matches!(rpc(&mut control, 4, Message::ApplySettings { expected_revision: 0, route: route.clone() }).await, Message::Status { revision: 1, running: [Some(config), None] } if config == mic)
    );
    assert!(matches!(
        rpc(
            &mut control,
            5,
            Message::ApplySettings {
                expected_revision: 0,
                route: Route::PhoneOnly
            }
        )
        .await,
        Message::Error {
            code: Code::RevisionConflict
        }
    ));
    assert!(matches!(
        rpc(
            &mut control,
            6,
            Message::ApplySettings {
                expected_revision: 1,
                route: Route::Mirror {
                    name: Some("litz.microphone".into())
                }
            }
        )
        .await,
        Message::Error {
            code: Code::RouteConflict
        }
    ));
    let Message::Capabilities {
        desktop: Some(inventory),
        ..
    } = rpc(&mut control, 7, capabilities()).await
    else {
        panic!("missing actual route")
    };
    assert_eq!(inventory.effective, Some(route));
    assert_eq!(inventory.revision, 1);
    assert!(
        matches!(rpc(&mut control, 8, Message::ApplySettings { expected_revision: 1, route: Route::PhoneOnly }).await, Message::Status { revision: 2, running: [Some(config), None] } if config == mic)
    );
    let Message::Capabilities {
        desktop: Some(inventory),
        ..
    } = rpc(&mut control, 9, capabilities()).await
    else {
        panic!("missing phone sink route")
    };
    assert_eq!(inventory.effective, Some(Route::PhoneOnly));
    let playback = Config {
        stream: Stream::Playback,
        epoch: 1,
    };
    assert!(
        matches!(rpc(&mut control, 10, Message::Start(playback)).await, Message::Status { running: [Some(config),Some(other)], .. } if config == mic && other == playback)
    );
    assert!(matches!(
        rpc(
            &mut control,
            11,
            Message::ApplySettings {
                expected_revision: 2,
                route: Route::Mirror {
                    name: Some("test.output".into())
                }
            }
        )
        .await,
        Message::Error {
            code: Code::RouteConflict
        }
    ));
    assert!(
        matches!(rpc(&mut control, 12, Message::GetState {}).await, Message::Status { revision: 2, running: [Some(config),Some(other)] } if config == mic && other == playback)
    );
    assert!(
        matches!(rpc(&mut control, 13, Message::Stop { stream: Stream::Playback }).await, Message::Status { running: [Some(config),None], .. } if config == mic)
    );
    assert!(
        fixture.children[1].try_wait().unwrap().is_none(),
        "agent exited during routing"
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("litz/settings.json")).unwrap()).unwrap();
    assert_eq!(persisted["revision"], 2);
    assert_eq!(persisted["route"], "PhoneOnly");
    let settings = litz_core::protocol::VoiceSettings {
        suppression: litz_core::protocol::Suppression::Rnnoise,
        ..Default::default()
    };
    assert!(matches!(rpc(&mut control, 14, Message::ApplyVoice {
        expected_revision: 2, voice: settings,
    }).await, Message::Status { revision: 3, running: [Some(config), None] } if config == mic));
    assert!(matches!(
        rpc(
            &mut control,
            15,
            Message::ApplyVoice {
                expected_revision: 2,
                voice: settings,
            }
        )
        .await,
        Message::Error {
            code: Code::RevisionConflict
        }
    ));
    let set_default = |name: &str| {
        let value = serde_json::json!({"name": name}).to_string();
        assert!(
            Command::new("pw-metadata")
                .args([
                    "-n",
                    "default",
                    "0",
                    "default.audio.sink",
                    &value,
                    "Spa:String:JSON"
                ])
                .env("XDG_RUNTIME_DIR", &directory)
                .env("PIPEWIRE_RUNTIME_DIR", &directory)
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    };
    set_default("test.output");
    assert!(matches!(
        rpc(
            &mut control,
            16,
            Message::ApplySettings {
                expected_revision: 3,
                route: Route::Mirror { name: None }
            }
        )
        .await,
        Message::Status { revision: 4, .. }
    ));
    let playback = Config {
        stream: Stream::Playback,
        epoch: 2,
    };
    assert!(
        matches!(rpc(&mut control, 17, Message::Start(playback)).await, Message::Status { running: [Some(a), Some(b)], .. } if a == mic && b == playback)
    );
    set_default("test.output.two");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let record =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), control.read())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(record.request_id, 0);
        if let Message::Capabilities {
            desktop: Some(desktop),
            ..
        } = record.message
        {
            if desktop.resolved.as_deref() == Some("test.output.two") {
                assert_eq!(desktop.effective, Some(Route::Mirror { name: None }));
                assert_eq!(desktop.revision, 4);
                break;
            }
        } else {
            panic!("default output change stopped audio");
        }
    }
    assert!(
        matches!(rpc(&mut control, 18, Message::GetState {}).await, Message::Status { running: [Some(a), Some(b)], .. } if a == mic && b == playback)
    );
    for (name, resolved) in [
        ("missing.output", None),
        ("test.output", Some("test.output")),
    ] {
        set_default(name);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let record = tokio::time::timeout_at(deadline, control.read())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(record.request_id, 0);
            let Message::Capabilities {
                desktop: Some(desktop),
                ..
            } = record.message
            else {
                panic!("temporary default loss stopped audio");
            };
            if desktop.resolved.as_deref() == resolved {
                assert_eq!(desktop.effective, Some(Route::Mirror { name: None }));
                assert_eq!(desktop.revision, 4);
                break;
            }
        }
    }
    assert!(
        matches!(rpc(&mut control, 18, Message::GetState {}).await, Message::Status { running: [Some(a), Some(b)], .. } if a == mic && b == playback)
    );
    assert!(matches!(
        rpc(
            &mut control,
            19,
            Message::Stop {
                stream: Stream::Playback
            }
        )
        .await,
        Message::Status { .. }
    ));
    let second = rcgen::generate_simple_self_signed(vec!["litz.local".into()]).unwrap();
    let mut other_endpoint = transport::client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    other_endpoint.set_default_client_config(
        transport::client(
            cert,
            second.cert.der().to_vec(),
            second.signing_key.serialize_der(),
        )
        .unwrap(),
    );
    // Avoid fixture bursts hitting the production handshake admission limit.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let provisional = other_endpoint
        .connect(address, "litz.local")
        .unwrap()
        .await
        .unwrap();
    enroll(&provisional, &mut fixture).await;
    let other = other_endpoint
        .connect(address, "litz.local")
        .unwrap()
        .await
        .unwrap();
    let mut other_control = Control::new(other.open_bi().await.unwrap());
    assert!(matches!(
        rpc(&mut other_control, 1, Message::GetState {}).await,
        Message::Status { .. }
    ));
    let Message::Ownership {
        generation,
        owned: false,
        owner: Some(_),
    } = rpc(&mut other_control, 2, Message::GetOwnership {}).await
    else {
        panic!("owner missing")
    };
    assert!(matches!(
        rpc(
            &mut other_control,
            3,
            Message::Acquire {
                expected_generation: None
            }
        )
        .await,
        Message::Ownership { owned: false, .. }
    ));
    assert!(matches!(
        rpc(
            &mut other_control,
            4,
            Message::Acquire {
                expected_generation: Some(generation - 1)
            }
        )
        .await,
        Message::Ownership { owned: false, .. }
    ));
    assert!(matches!(
        rpc(&mut control, 14, Message::GetState {}).await,
        Message::Status {
            running: [Some(_), None],
            ..
        }
    ));
    assert!(matches!(
        rpc(
            &mut other_control,
            5,
            Message::Acquire {
                expected_generation: Some(generation)
            }
        )
        .await,
        Message::Ownership { owned: true, .. }
    ));
    assert!(matches!(
        rpc(&mut other_control, 6, Message::Start(mic)).await,
        Message::Status {
            running: [Some(_), None],
            ..
        }
    ));
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(3), connection.closed()).await.unwrap(), quinn::ConnectionError::ApplicationClosed(close) if close.error_code == quinn::VarInt::from_u32(0x20))
    );
    assert!(matches!(
        rpc(&mut other_control, 7, Message::Release {}).await,
        Message::Ownership {
            owner: None,
            owned: false,
            ..
        }
    ));
    let reconnected = endpoint
        .connect(address, "litz.local")
        .unwrap()
        .await
        .unwrap();
    let mut reconnected_control = Control::new(reconnected.open_bi().await.unwrap());
    assert!(matches!(
        rpc(&mut reconnected_control, 1, Message::GetState {}).await,
        Message::Status {
            running: [None, None],
            ..
        }
    ));
    other_control
        .write(&Record::new(8, Message::Unpair {}))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(3), other.closed())
        .await
        .unwrap();
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("identity.json")).unwrap()).unwrap();
    assert_eq!(persisted["trust"]["phones"].as_array().unwrap().len(), 1);
    let revoked = other_endpoint
        .connect(address, "litz.local")
        .unwrap()
        .await
        .unwrap();
    let mut revoked_control = Control::new(revoked.open_bi().await.unwrap());
    assert!(matches!(
        rpc(&mut revoked_control, 1, Message::GetState {}).await,
        Message::Error {
            code: Code::Unauthorized
        }
    ));
    let forget = litzd(&["forget", "Test phone"]);
    assert!(forget.status.success(), "{forget:?}");
    tokio::time::timeout(Duration::from_secs(3), reconnected.closed())
        .await
        .unwrap();
    let devices = litzd(&["devices"]);
    assert!(String::from_utf8_lossy(&devices.stdout).contains("No phones are paired"));
    connection.close(0u8.into(), b"test complete");
}
