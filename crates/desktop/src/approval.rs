use litz_core::protocol::APPROVAL_WINDOW;
use std::net::IpAddr;

pub struct Request<'a> {
    pub name: &'a str,
    pub address: IpAddr,
    pub code: &'a str,
    /// Name and last-seen time of the phone that allowing this forgets.
    pub replaces: Option<(&'a str, u64)>,
}

impl Request<'_> {
    fn question(&self) -> String {
        format!("Allow “{}” ({}) to connect?", self.name, self.address)
    }

    fn details(&self) -> String {
        let mut text = format!("Check that the phone shows {}.", self.code);
        if let Some((name, last_seen)) = self.replaces {
            let days = now().saturating_sub(last_seen) / 86_400;
            text += &format!(
                "\n\nAll {} pairing slots are in use. Allowing forgets “{name}”, last used {days} days ago.",
                litz_core::session::MAX_PHONES
            );
        }
        text
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(target_os = "linux")]
pub async fn approve(request: &Request<'_>) -> bool {
    match notification::ask(request).await {
        Ok(answer) => answer,
        Err(error) => {
            eprintln!("litzd: no desktop notification ({error}); asking on the terminal");
            terminal(request).await
        }
    }
}

#[cfg(target_os = "linux")]
async fn terminal(request: &Request<'_>) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    let prompt = format!("{}\n{}\n[y/N] ", request.question(), request.details());
    let answer = tokio::task::spawn_blocking(move || {
        if std::io::stdin().is_terminal() {
            let _ = write!(std::io::stderr(), "{prompt}");
        }
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => false,
            Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        }
    });
    matches!(
        tokio::time::timeout(APPROVAL_WINDOW, answer).await,
        Ok(Ok(true))
    )
}

#[cfg(target_os = "linux")]
mod notification {
    use super::{APPROVAL_WINDOW, Request};
    use futures_lite::StreamExt;
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    /// Notification servers may render markup in the body.
    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// `Err` only when no server can show buttons; dismissing the
    /// notification or letting it expire is a refusal.
    pub async fn ask(request: &Request<'_>) -> zbus::Result<bool> {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
        if !capabilities.iter().any(|c| c == "actions") {
            return Err(zbus::Error::Unsupported);
        }
        // One stream keeps bus order: servers send ActionInvoked and then
        // NotificationClosed, and separate streams could see the close first.
        let mut signals = proxy.receive_all_signals().await?;
        // Critical urgency keeps the notification on screen until answered.
        let hints = HashMap::from([("urgency", Value::U8(2))]);
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "Litz Audio",
                    0u32,
                    "audio-input-microphone",
                    escape(&request.question()),
                    escape(&request.details()),
                    ["allow", "Allow", "deny", "Deny"].as_slice(),
                    hints,
                    0i32,
                ),
            )
            .await?;
        let answer = async {
            while let Some(message) = signals.next().await {
                let header = message.header();
                match header.member().map(|member| member.as_str()) {
                    Some("ActionInvoked") => {
                        let (signal_id, action): (u32, String) = message.body().deserialize()?;
                        if signal_id == id {
                            return Ok(action == "allow");
                        }
                    }
                    Some("NotificationClosed") => {
                        let (signal_id, _reason): (u32, u32) = message.body().deserialize()?;
                        if signal_id == id {
                            return Ok(false);
                        }
                    }
                    _ => {}
                }
            }
            Ok(false)
        };
        match tokio::time::timeout(APPROVAL_WINDOW, answer).await {
            Ok(result) => result,
            Err(_) => {
                let _: zbus::Result<()> = proxy.call("CloseNotification", &(id,)).await;
                Ok(false)
            }
        }
    }
}

#[cfg(windows)]
pub async fn approve(request: &Request<'_>) -> bool {
    let (question, details) = (request.question(), request.details());
    let answer = tokio::task::spawn_blocking(move || match toast::ask(&question, &details) {
        Ok(answer) => answer,
        Err(error) => {
            eprintln!("litzd: no notification ({error}); asking in a window");
            message_box(&format!("{question}\n\n{details}"))
        }
    });
    matches!(answer.await, Ok(true))
}

#[cfg(windows)]
mod toast {
    use super::APPROVAL_WINDOW;
    use std::sync::mpsc;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
    };
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::Win32::UI::Shell::{QUNS_ACCEPTS_NOTIFICATIONS, SHQueryUserNotificationState};
    use windows::core::{HSTRING, IInspectable, Interface, PCWSTR, Ref, w};

    const APP_ID: &str = "LitzAudio";

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    fn set(name: PCWSTR, value: &str) -> windows::core::Result<()> {
        let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
        // SAFETY: the subkey and name are static NUL-terminated strings and
        // `value` is a live NUL-terminated buffer of the given byte length.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Classes\\AppUserModelId\\LitzAudio"),
                name,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            )
            .ok()
        }
    }

    fn register() -> windows::core::Result<()> {
        set(w!("DisplayName"), "Litz Audio")?;
        if let Some(icon) = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("litz.png")))
            .filter(|icon| icon.is_file())
        {
            set(w!("IconUri"), &icon.to_string_lossy())?;
        }
        Ok(())
    }

    pub fn ask(question: &str, details: &str) -> windows::core::Result<bool> {
        // SAFETY: called on a blocking-pool thread; an existing apartment of
        // either kind is fine because only agile WinRT objects are used.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        // SAFETY: no arguments; the call only reads shell state.
        if unsafe { SHQueryUserNotificationState()? } != QUNS_ACCEPTS_NOTIFICATIONS {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_ABORT,
                "notifications are silenced",
            ));
        }
        register()?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?;
        if notifier.Setting()? != NotificationSetting::Enabled {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_ACCESSDENIED,
                "notifications are turned off for Litz",
            ));
        }
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            r#"<toast scenario="reminder"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual><actions><action content="Allow" arguments="allow" activationType="foreground"/><action content="Deny" arguments="deny" activationType="foreground"/></actions></toast>"#,
            escape(question),
            escape(details)
        )))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        let (sender, receiver) = mpsc::channel();
        let activated = sender.clone();
        toast.Activated(&TypedEventHandler::new(
            move |_, args: Ref<IInspectable>| {
                let allow = args.ok()?.cast::<ToastActivatedEventArgs>()?.Arguments()? == "allow";
                let _ = activated.send(Ok(allow));
                Ok(())
            },
        ))?;
        let dismissed = sender.clone();
        toast.Dismissed(&TypedEventHandler::new(move |_, _| {
            let _ = dismissed.send(Ok(false));
            Ok(())
        }))?;
        toast.Failed(&TypedEventHandler::new(move |_, _| {
            let _ = sender.send(Err(windows::core::Error::from(
                windows::Win32::Foundation::E_FAIL,
            )));
            Ok(())
        }))?;
        notifier.Show(&toast)?;
        let answer = receiver.recv_timeout(APPROVAL_WINDOW);
        let _ = notifier.Hide(&toast);
        answer.unwrap_or(Ok(false))
    }
}

#[cfg(windows)]
fn message_box(text: &str) -> bool {
    use std::sync::mpsc;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
        PostMessageW, WM_CLOSE,
    };

    let title: Vec<u16> = "Litz Audio\0".encode_utf16().collect();
    let body: Vec<u16> = format!("{text}\0").encode_utf16().collect();
    let caption = title.clone();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        // SAFETY: both NUL-terminated UTF-16 buffers outlive the call.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
        let _ = sender.send(answer);
    });
    match receiver.recv_timeout(APPROVAL_WINDOW) {
        Ok(answer) => answer == IDYES,
        Err(_) => {
            // SAFETY: the caption is a live NUL-terminated buffer; the handle
            // is only used to post a close message.
            unsafe {
                let window = FindWindowW(std::ptr::null(), caption.as_ptr());
                if !window.is_null() {
                    PostMessageW(window, WM_CLOSE, 0, 0);
                }
            }
            false
        }
    }
}
