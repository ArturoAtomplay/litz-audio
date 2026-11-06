use litz_core::protocol::Route;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    pub revision: u64,
    pub route: Option<Route>,
    #[serde(default)]
    pub voice: litz_core::protocol::VoiceSettings,
}
pub fn load() -> Result<Settings, Box<dyn std::error::Error>> {
    let path = crate::identity::directory()?.join("settings.json");
    match crate::platform::read_limited(&path, 4096) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn save(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    write(
        &crate::identity::directory()?.join("settings.json"),
        settings,
    )
}
fn write(path: &Path, settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    crate::platform::write_atomic(path, &serde_json::to_vec(settings)?)?;
    crate::platform::sync_parent(path.parent().ok_or("settings parent")?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn route_commit_keeps_stable_names_and_revision() {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let dir =
            std::env::temp_dir().join(format!("litz-settings-{:016x}", u64::from_ne_bytes(nonce)));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.json");
        let settings = Settings {
            voice: Default::default(),
            revision: 7,
            route: Some(Route::Mirror {
                name: Some("alsa_output.physical".into()),
            }),
        };
        write(&path, &settings).unwrap();
        let restored: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(restored.revision, 7);
        assert_eq!(restored.route, settings.route);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
