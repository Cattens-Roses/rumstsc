//! A small versioned line format. Credentials are never persisted.
use crate::theme::Theme;
use std::{fs, io, path::PathBuf};

#[derive(Default)]
pub struct Settings {
    pub theme: Theme,
    pub recent: Vec<String>,
    pub resolution: usize,
    pub native_display: bool,
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
        let mut settings = Self::default();
        let Some(data) = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .filter(|s| s.len() < 16384)
        else {
            return settings;
        };
        for line in data.lines() {
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
        settings
    }
    pub fn remember(&mut self, host: String) {
        self.recent.retain(|h| h != &host);
        self.recent.insert(0, host);
        self.recent.truncate(5);
    }
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = path() else {
            return Ok(());
        };
        fs::create_dir_all(path.parent().unwrap())?;
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
        for host in &self.recent {
            if !host.contains(['\n', '\r']) {
                data.push_str(&format!("host={host}\n"));
            }
        }
        fs::write(path, data)
    }
}
