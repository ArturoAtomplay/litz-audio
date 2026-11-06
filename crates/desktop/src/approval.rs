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
    let text = format!("{}\n\n{}", request.question(), request.details());
    matches!(
        tokio::task::spawn_blocking(move || message_box(&text)).await,
        Ok(true)
    )
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
