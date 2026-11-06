use mdns_sd::{ServiceDaemon, ServiceInfo};

pub struct Publisher(Option<ServiceDaemon>);
impl Publisher {
    pub fn try_new(name: &str, id: &str, port: u16) -> litz_core::transport::Result<Self> {
        // Private integration fixtures must never advertise on the developer LAN.
        if std::env::var_os("LITZ_DISABLE_DISCOVERY").is_some() {
            return Ok(Self(None));
        }
        let host = format!("litz-{}.local.", &id[..16]);
        let info = ServiceInfo::new(
            "_litz._udp.local.",
            name,
            &host,
            "",
            port,
            &[
                ("v", litz_core::protocol::VERSION.to_string().as_str()),
                ("id", id),
            ][..],
        )?
        .enable_addr_auto();
        let daemon = ServiceDaemon::new()?;
        if let Err(error) = daemon.register(info) {
            let _ = daemon.shutdown();
            return Err(error.into());
        }
        Ok(Self(Some(daemon)))
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        if let Some(daemon) = &self.0 {
            let _ = daemon.shutdown();
        }
    }
}
