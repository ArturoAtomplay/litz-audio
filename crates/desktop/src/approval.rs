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
pub async fn approve(request: &Request<'_>) -> Option<bool> {
    match notification::ask(request).await {
        Ok(answer) => Some(answer),
        Err(error) => {
            eprintln!("litzd: no desktop notification ({error}); asking on the terminal");
            terminal(request).await
        }
    }
}

#[cfg(target_os = "linux")]
async fn terminal(request: &Request<'_>) -> Option<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    let prompt = format!("{}\n{}\n[y/N] ", request.question(), request.details());
    let answer = tokio::task::spawn_blocking(move || {
        if std::io::stdin().is_terminal() {
            let _ = write!(std::io::stderr(), "{prompt}");
        }
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(matches!(
                line.trim().to_ascii_lowercase().as_str(),
                "y" | "yes"
            )),
        }
    });
    match tokio::time::timeout(APPROVAL_WINDOW, answer).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => None,
        Err(_) => Some(false),
    }
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
pub async fn approve(request: &Request<'_>) -> Option<bool> {
    let (question, details) = (request.question(), request.details());
    match tokio::task::spawn_blocking(move || toast::ask(&question, &details)).await {
        Ok(Ok(answer)) => Some(answer),
        Ok(Err(error)) => {
            eprintln!("litzd: approval notification failed: {error}");
            None
        }
        Err(_) => None,
    }
}

#[cfg(windows)]
mod toast {
    use super::APPROVAL_WINDOW;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, mpsc};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastDismissalReason,
        ToastDismissedEventArgs, ToastNotification, ToastNotificationManager,
    };
    use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_POINTER};
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, CoIncrementMTAUsage, CoRegisterClassObject, IClassFactory,
        IClassFactory_Impl, REGCLS_MULTIPLEUSE,
    };
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::Win32::UI::Notifications::{
        INotificationActivationCallback, INotificationActivationCallback_Impl,
        NOTIFICATION_USER_INPUT_DATA,
    };
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::{
        BOOL, Error, GUID, HSTRING, IInspectable, IUnknown, Interface, PCWSTR, Ref, Result,
        implement, w,
    };

    const APP_ID: PCWSTR = w!("LitzAudio");
    const ACTIVATOR: GUID = GUID::from_u128(0xBFD99F34_90AE_4D6D_A33A_6EB4D6FCC2A4);

    static PENDING: Mutex<Option<(u64, mpsc::Sender<Result<bool>>)>> = Mutex::new(None);

    fn reply(arguments: &str) {
        let Some((choice, id)) = arguments.split_once(' ') else {
            return;
        };
        let mut pending = PENDING.lock().unwrap();
        if pending
            .as_ref()
            .is_some_and(|(current, _)| id == current.to_string())
            && let Some((_, sender)) = pending.take()
        {
            let _ = sender.send(Ok(choice == "allow"));
        }
    }

    #[implement(INotificationActivationCallback)]
    struct Activator;

    impl INotificationActivationCallback_Impl for Activator_Impl {
        fn Activate(
            &self,
            _: &PCWSTR,
            arguments: &PCWSTR,
            _: *const NOTIFICATION_USER_INPUT_DATA,
            _: u32,
        ) -> Result<()> {
            if !arguments.is_null() {
                // SAFETY: COM passes a NUL-terminated string valid for this call.
                if let Ok(arguments) = unsafe { arguments.to_string() } {
                    reply(&arguments);
                }
            }
            Ok(())
        }
    }

    #[implement(IClassFactory)]
    struct Factory;

    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(
            &self,
            outer: Ref<IUnknown>,
            iid: *const GUID,
            object: *mut *mut c_void,
        ) -> Result<()> {
            if object.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: COM supplies a writable interface out pointer.
            unsafe { *object = std::ptr::null_mut() };
            if outer.as_ref().is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let activator: INotificationActivationCallback = Activator.into();
            // SAFETY: `iid` and `object` are COM's CreateInstance arguments.
            unsafe { activator.query(iid, object).ok() }
        }

        fn LockServer(&self, _: BOOL) -> Result<()> {
            Ok(())
        }
    }

    fn set(key: PCWSTR, name: PCWSTR, value: &str) -> Result<()> {
        let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
        // SAFETY: `key` and `name` are NUL-terminated or null, and `value` is a
        // live NUL-terminated buffer of the given byte length.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                name,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            )
            .ok()
        }
    }

    fn register() -> Result<()> {
        let exe = std::env::current_exe().map_err(|error| Error::new(E_FAIL, error.to_string()))?;
        let app = w!("Software\\Classes\\AppUserModelId\\LitzAudio");
        set(app, w!("DisplayName"), "Litz Audio")?;
        set(app, w!("CustomActivator"), &format!("{{{ACTIVATOR:?}}}"))?;
        if let Some(icon) = exe
            .parent()
            .map(|dir| dir.join("litz.png"))
            .filter(|icon| icon.is_file())
        {
            set(app, w!("IconUri"), &icon.to_string_lossy())?;
        }
        set(
            w!("Software\\Classes\\CLSID\\{BFD99F34-90AE-4D6D-A33A-6EB4D6FCC2A4}\\LocalServer32"),
            PCWSTR::null(),
            &format!("\"{}\" -ToastActivated", exe.display()),
        )?;
        // SAFETY: APP_ID is a static NUL-terminated string. The MTA usage is
        // never released, so the class object stays reachable for the
        // process lifetime.
        unsafe {
            SetCurrentProcessExplicitAppUserModelID(APP_ID)?;
            CoIncrementMTAUsage()?;
            let factory: IClassFactory = Factory.into();
            CoRegisterClassObject(
                &ACTIVATOR,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE,
            )?;
        }
        Ok(())
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }

    pub fn ask(question: &str, details: &str) -> Result<bool> {
        static REGISTERED: OnceLock<()> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        if REGISTERED.get().is_none() {
            register()?;
            let _ = REGISTERED.set(());
        }
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("LitzAudio"))?;
        if notifier
            .Setting()
            .is_ok_and(|setting| setting != NotificationSetting::Enabled)
        {
            return Err(Error::new(E_FAIL, "notifications are turned off"));
        }
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        *PENDING.lock().unwrap() = Some((id, sender.clone()));
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            r#"<toast scenario="reminder" launch="deny {id}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual><actions><action content="Allow" arguments="allow {id}" activationType="foreground"/><action content="Deny" arguments="deny {id}" activationType="foreground"/></actions></toast>"#,
            escape(question),
            escape(details)
        )))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.Activated(&TypedEventHandler::new(|_, args: Ref<IInspectable>| {
            reply(
                &args
                    .ok()?
                    .cast::<ToastActivatedEventArgs>()?
                    .Arguments()?
                    .to_string(),
            );
            Ok(())
        }))?;
        toast.Dismissed(&TypedEventHandler::new(
            move |_, args: Ref<ToastDismissedEventArgs>| {
                if args.ok()?.Reason()? == ToastDismissalReason::UserCanceled {
                    reply(&format!("deny {id}"));
                }
                Ok(())
            },
        ))?;
        toast.Failed(&TypedEventHandler::new(move |_, _| {
            let _ = sender.send(Err(E_FAIL.into()));
            Ok(())
        }))?;
        notifier.Show(&toast)?;
        let answer = receiver.recv_timeout(APPROVAL_WINDOW);
        PENDING.lock().unwrap().take();
        let _ = notifier.Hide(&toast);
        answer.unwrap_or(Ok(false))
    }
}
