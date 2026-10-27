//! A small versioned line format. Credentials are never persisted.
use crate::theme::Theme;
use std::{fs, io, path::PathBuf};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PowerProfile {
    #[default]
    Smooth,
    Balanced,
    Battery,
}
impl PowerProfile {
    pub fn interval_ms(self) -> u64 {
        match self {
            Self::Smooth => 17,
            Self::Balanced => 34,
            Self::Battery => 67,
        }
    }
}
pub struct Settings {
    pub theme: Theme,
    pub recent: Vec<String>,
    pub resolution: usize,
    pub native_display: bool,
    pub power_profile: PowerProfile,
    pub pause_background: bool,
    pub remember_recent: bool,
    pub show_status: bool,
    pub start_fullscreen: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            recent: vec![],
            resolution: 0,
            native_display: false,
            power_profile: PowerProfile::Smooth,
            pause_background: true,
            remember_recent: true,
            show_status: true,
            start_fullscreen: false,
        }
    }
}
fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(if cfg!(windows) { "APPDATA" } else { "HOME" }).map(|p| {
                let mut p = PathBuf::from(p);
                if !cfg!(windows) {
                    p.push(".config");
                }
                p
            })
        })
        .map(|p| p.join("rumstsc").join("settings"))
}
impl Settings {
    pub fn startup_trace_path() -> Option<PathBuf> {
        Self::trace_path().map(|p| p.with_file_name("startup-trace.json"))
    }
    pub fn trace_path() -> Option<PathBuf> {
        if std::env::var_os("RUMSTSC_TRACE").is_some_and(|v| v == "1") {
            path().map(|p| p.with_file_name("last-load-trace.json"))
        } else {
            None
        }
    }
    pub fn load() -> Self {
        let Some(data) = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .filter(|s| s.len() < 16384)
        else {
            return Self::default();
        };
        Self::parse(&data)
    }
    fn parse(data: &str) -> Self {
        let mut settings = Self::default();
        for line in data.lines() {
            match line {
                "power=balanced" => settings.power_profile = PowerProfile::Balanced,
                "power=battery" => settings.power_profile = PowerProfile::Battery,
                "background_pause=false" => settings.pause_background = false,
                "remember_recent=false" => settings.remember_recent = false,
                "show_status=false" => settings.show_status = false,
                "start_fullscreen=true" => settings.start_fullscreen = true,
                _ => {}
            }
            if line == "display=native" {
                settings.native_display = true;
            }
            if line == "theme=light" {
                settings.theme = Theme::Light;
            }
            if let Some(n) = line
                .strip_prefix("resolution=")
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n < 3)
            {
                settings.resolution = n;
            }
            if let Some(host) = line
                .strip_prefix("host=")
                .filter(|s| s.len() < 256 && rumstsc_rdp::negotiation::Endpoint::parse(s).is_ok())
                && settings.recent.len() < 5
                && !settings.recent.iter().any(|h| h == host)
            {
                settings.recent.push(host.into());
            }
        }
        if !settings.remember_recent {
            settings.recent.clear();
        }
        settings
    }
    pub fn remember(&mut self, host: String) {
        if !self.remember_recent {
            return;
        }
        self.recent.retain(|h| h != &host);
        self.recent.insert(0, host);
        self.recent.truncate(5);
    }
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = path() else {
            return Ok(());
        };
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, self.encode())
    }
    fn encode(&self) -> String {
        let mut data = format!(
            "version=1\ntheme={}\nresolution={}\n",
            if self.theme == Theme::Light {
                "light"
            } else {
                "dark"
            },
            self.resolution
        );
        data.push_str(if self.native_display {
            "display=native\n"
        } else {
            "display=gpui\n"
        });
        data.push_str(&format!("power={}\nbackground_pause={}\nremember_recent={}\nshow_status={}\nstart_fullscreen={}\n",
            match self.power_profile { PowerProfile::Smooth => "smooth", PowerProfile::Balanced => "balanced", PowerProfile::Battery => "battery" },
            self.pause_background, self.remember_recent, self.show_status, self.start_fullscreen));
        for host in self.recent.iter().filter(|_| self.remember_recent) {
            if !host.contains(['\n', '\r']) {
                data.push_str(&format!("host={host}\n"));
            }
        }
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_keep_connection_preferences_and_safe_defaults() {
        let settings = Settings::parse(
            "version=1\ntheme=light\nresolution=2\ndisplay=native\nhost=office.example\n",
        );
        assert!(settings.theme == Theme::Light && settings.native_display);
        assert_eq!(settings.resolution, 2);
        assert!(settings.pause_background && settings.remember_recent && settings.show_status);
        assert!(!settings.start_fullscreen);
        assert!(settings.power_profile == PowerProfile::Smooth);
        let invalid = Settings::parse("resolution=999\npower=unknown\nhost=\n");
        assert_eq!(invalid.resolution, 0);
        assert!(invalid.recent.is_empty());
    }
    #[test]
    fn preferences_roundtrip_and_history_opt_out() {
        let original = Settings {
            theme: Theme::Light,
            recent: vec!["office.example".into()],
            resolution: 2,
            native_display: true,
            power_profile: PowerProfile::Battery,
            pause_background: false,
            remember_recent: true,
            show_status: false,
            start_fullscreen: true,
        };
        let encoded = original.encode();
        let mut restored = Settings::parse(&encoded);
        assert!(restored.theme == Theme::Light && restored.power_profile == PowerProfile::Battery);
        assert!(restored.native_display && restored.start_fullscreen);
        assert!(!restored.pause_background && !restored.show_status);
        assert_eq!(restored.recent, ["office.example"]);
        restored.remember_recent = false;
        restored.remember("another.example".into());
        assert!(!restored.encode().contains("host="));
        assert!(
            Settings::parse(&(encoded + "remember_recent=false\n"))
                .recent
                .is_empty()
        );
        assert!(!restored.encode().contains("password"));
        // Each cadence stays below its advertised upper bound.
        for (profile, fps) in [
            (PowerProfile::Smooth, 60),
            (PowerProfile::Balanced, 30),
            (PowerProfile::Battery, 15),
        ] {
            assert!(1000.0 / profile.interval_ms() as f64 <= fps as f64);
        }
    }
}
