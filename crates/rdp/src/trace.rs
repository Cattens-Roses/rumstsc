//! Bounded, payload-free timings exportable as Chrome Trace JSON.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Write},
    path::Path,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
const LIMIT: usize = 32768;
static LATEST: OnceLock<Mutex<BTreeMap<PathBuf, u64>>> = OnceLock::new();
static EXPORTS: Mutex<()> = Mutex::new(());
fn latest() -> &'static Mutex<BTreeMap<PathBuf, u64>> {
    LATEST.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[derive(Clone)]
struct Entry {
    name: &'static str,
    lane: &'static str,
    start: u64,
    duration: u64,
    value: u64,
}
pub struct Trace {
    id: u64,
    enabled: bool,
    epoch: Instant,
    entries: Mutex<Vec<Entry>>,
}
pub struct Span<'a> {
    trace: &'a Trace,
    name: &'static str,
    lane: &'static str,
    start: Option<Instant>,
    value: u64,
}
impl Default for Trace {
    fn default() -> Self {
        Self::new(true)
    }
}
impl Trace {
    pub fn new(enabled: bool) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            enabled,
            epoch: Instant::now(),
            entries: Mutex::new(Vec::new()),
        }
    }
}
impl Trace {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn autosave(trace: &std::sync::Arc<Self>, path: std::path::PathBuf) {
        if !trace.enabled {
            return;
        }
        latest().lock().unwrap().insert(path.clone(), trace.id);
        let weak = std::sync::Arc::downgrade(trace);
        let _ = std::thread::Builder::new()
            .name("rdp-trace-writer".into())
            .spawn(move || {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                for _ in 0..30 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let Some(trace) = weak.upgrade() else {
                        break;
                    };
                    let _ = trace.save(&path);
                }
            });
    }
    pub fn span(&self, name: &'static str, lane: &'static str, value: u64) -> Span<'_> {
        Span {
            trace: self,
            name,
            lane,
            value,
            start: self.enabled.then(Instant::now),
        }
    }
    pub fn mark(&self, name: &'static str, lane: &'static str, value: u64) {
        if self.enabled {
            self.record(name, lane, Instant::now(), 0, value);
        }
    }
    fn record(
        &self,
        name: &'static str,
        lane: &'static str,
        start: Instant,
        duration: u64,
        value: u64,
    ) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() < LIMIT {
            entries.push(Entry {
                name,
                lane,
                start: start.duration_since(self.epoch).as_micros() as u64,
                duration,
                value,
            });
        }
    }
    /// Entries contain fixed operation names and numeric counts only. Never
    /// record endpoints, certificates, credentials, keystrokes or screen pixels.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let _export = EXPORTS.lock().unwrap();
        // A previous connection must not overwrite the next connection's trace.
        if latest()
            .lock()
            .unwrap()
            .get(path)
            .is_some_and(|id| *id > self.id)
        {
            return Ok(());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let entries = self.entries.lock().unwrap().clone();
        let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), self.id));
        let mut file = io::BufWriter::new(File::create(&temporary)?);
        write!(file, "{{\"traceEvents\":[")?;
        for (i, e) in entries.iter().enumerate() {
            if i > 0 {
                write!(file, ",")?;
            }
            // Escape even fixed labels, keeping this writer safe for future names.
            let escape = |s: &str| {
                s.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t")
            };
            write!(
                file,
                "{{\"name\":\"{}\",\"cat\":\"rdp\",\"ph\":\"X\",\"pid\":1,\"tid\":{},\"ts\":{},\"dur\":{},\"args\":{{\"value\":{}}}}}",
                escape(e.name),
                match e.lane {
                    "network" => 1,
                    "graphics" => 2,
                    "ui" => 3,
                    "native" => 5,
                    _ => 4,
                },
                e.start,
                e.duration,
                e.value
            )?;
        }
        for (index, name) in ["network", "graphics", "ui", "other", "native"]
            .iter()
            .enumerate()
        {
            if !entries.is_empty() || index > 0 {
                write!(file, ",")?;
            }
            write!(
                file,
                "{{\"name\":\"thread_name\",\"ph\":\"M\",\"pid\":1,\"tid\":{},\"args\":{{\"name\":\"{}\"}}}}",
                index + 1,
                name
            )?;
        }
        write!(
            file,
            "],\"displayTimeUnit\":\"ms\",\"eventLimit\":{LIMIT}}}"
        )?;
        file.flush()?;
        drop(file);
        std::fs::rename(&temporary, path)
    }
}
impl Drop for Span<'_> {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            self.trace.record(
                self.name,
                self.lane,
                start,
                start.elapsed().as_micros() as u64,
                self.value,
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_trace_does_no_recording_or_io() {
        let t = Trace::new(false);
        let span = t.span("decode", "graphics", 1);
        assert!(span.start.is_none());
        drop(span);
        t.mark("read", "network", 1);
        assert!(t.entries.lock().unwrap().is_empty());
        t.save(Path::new("missing-parent/no-trace.json")).unwrap();
        assert!(!Path::new("missing-parent/no-trace.json").exists());
    }
    #[test]
    fn exports_replace_complete_files() {
        let trace = Trace::default();
        let path = std::env::temp_dir().join(format!(
            "rumstsc-trace-{}-{}.json",
            std::process::id(),
            trace.id
        ));
        trace.mark("first", "network", 1);
        trace.save(&path).unwrap();
        trace.mark("second", "graphics", 2);
        trace.save(&path).unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.starts_with("{\"traceEvents\":["));
        assert!(json.ends_with(&format!("\"eventLimit\":{LIMIT}}}")));
        assert!(json.contains("second"));
        assert!(
            !path
                .with_extension(format!("tmp-{}-{}", std::process::id(), trace.id))
                .exists()
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn timings_are_bounded_and_ordered() {
        let t = Trace::default();
        {
            let _span = t.span("decode", "graphics", 4);
        }
        for _ in 0..LIMIT + 1 {
            t.mark("read", "network", 16);
        }
        let e = t.entries.lock().unwrap();
        assert_eq!(e.len(), LIMIT);
        assert_eq!(e[0].name, "decode");
        assert_eq!(e[0].value, 4);
        assert!(e.windows(2).all(|w| w[0].start <= w[1].start));
    }
}
