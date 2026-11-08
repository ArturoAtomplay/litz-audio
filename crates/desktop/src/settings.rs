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
    read(&crate::identity::directory()?.join("settings.json"))
}
fn read(path: &Path) -> Result<Settings, Box<dyn std::error::Error>> {
    match crate::platform::read_limited(path, 4096) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            eprintln!("litzd: ignoring unreadable settings.json: {error}");
            Settings::default()
        })),
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
    #[test]
    fn settings_from_another_version_fall_back_to_defaults() {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let dir =
            std::env::temp_dir().join(format!("litz-settings-{:016x}", u64::from_ne_bytes(nonce)));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, br#"{"revision":"old","route":{"Removed":{}}}"#).unwrap();
        let restored = read(&path).unwrap();
        assert_eq!(restored.revision, 0);
        assert_eq!(restored.route, None);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
