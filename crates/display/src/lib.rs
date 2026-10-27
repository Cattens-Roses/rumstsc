//! Native child-window presentation. The UI sends tile deltas; this module
//! coalesces every pending tile, so a busy display cannot lose incremental pixels.
use rumstsc_rdp::{
    bitmap::{Snapshot, TilePixels},
    session::Input,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
};
#[cfg(windows)]
mod win;
#[cfg(target_os = "linux")]
mod x11;
#[derive(Clone, Copy)]
pub enum Parent {
    Windows(isize),
    X11(u32),
    X11Process(u32),
}
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
#[derive(Default)]
struct Mailbox {
    bounds: Bounds,
    generation: u64,
    revision: u64,
    width: u16,
    height: u16,
    tiles: BTreeMap<usize, TilePixels>,
    resize: bool,
}
struct Shared {
    mailbox: Mutex<Mailbox>,
    error: Mutex<Option<String>>,
    stop: AtomicBool,
    captured: AtomicBool,
    first_presented: AtomicBool,
    notified: AtomicBool,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    trace: Arc<rumstsc_rdp::trace::Trace>,
}
impl Shared {
    fn presented(&self, buffer: &Buffer) {
        if !buffer.pixels.is_empty()
            && buffer.viewport().width > 0
            && !self.first_presented.swap(true, Ordering::AcqRel)
        {
            self.trace.mark("first_frame_submitted", "native", 1);
        }
    }
}
#[derive(Clone)]
pub struct Handle(Arc<Shared>);
pub struct Display {
    handle: Handle,
}
impl Display {
    pub fn start(
        parent: Parent,
        input: SyncSender<Input>,
        trace: Arc<rumstsc_rdp::trace::Trace>,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            error: Mutex::new(None),
            stop: AtomicBool::new(false),
            captured: AtomicBool::new(true),
            first_presented: AtomicBool::new(false),
            notified: AtomicBool::new(false),
            wake: Mutex::new(None),
            trace,
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("rdp-native-display".into())
            .spawn(move || {
                let result = match parent {
                    #[cfg(windows)]
                    Parent::Windows(parent) => win::run(worker.clone(), parent, input),
                    #[cfg(target_os = "linux")]
                    Parent::X11(parent) => x11::run(worker.clone(), parent, input),
                    #[cfg(target_os = "linux")]
                    Parent::X11Process(pid) => x11::run_process(worker.clone(), pid, input),
                    _ => {
                        Err("Native framebuffer requires Windows or an X11/XWayland window".into())
                    }
                };
                if let Err(e) = result {
                    eprintln!("Native framebuffer unavailable: {e}");
                    *worker.error.lock().unwrap() = Some(e);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: Handle(shared),
        })
    }
    pub fn captured(&self) -> bool {
        self.handle.0.captured.load(Ordering::Acquire)
    }
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.handle.0.error.lock().unwrap().take()
    }
    pub fn submit(&self, snapshot: Snapshot) {
        if snapshot.width == 0
            || snapshot.height == 0
            || snapshot.width > 4096
            || snapshot.height > 2160
        {
            *self.handle.0.error.lock().unwrap() = Some("Native framebuffer dimensions".into());
            return;
        }
        let mut m = self.handle.0.mailbox.lock().unwrap();
        if m.generation != snapshot.generation {
            m.tiles.clear();
            m.resize = true;
        }
        m.generation = snapshot.generation;
        m.revision = snapshot.revision;
        m.width = snapshot.width;
        m.height = snapshot.height;
        for tile in snapshot.tiles {
            m.tiles.insert(tile.index, tile);
        }
        drop(m);
        self.handle.wake();
    }
}
impl Drop for Display {
    fn drop(&mut self) {
        self.handle.0.stop.store(true, Ordering::Release);
        self.handle.wake();
    }
}
impl Handle {
    pub fn bounds(&self, bounds: Bounds) {
        let mut m = self.0.mailbox.lock().unwrap();
        if m.bounds == bounds {
            return;
        }
        m.bounds = bounds;
        drop(m);
        self.wake();
    }
    fn wake(&self) {
        if !self.0.notified.swap(true, Ordering::AcqRel)
            && let Some(wake) = self.0.wake.lock().unwrap().as_ref()
        {
            wake();
        }
    }
}
#[derive(Default)]
struct Buffer {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    bounds: Bounds,
}
impl Buffer {
    fn receive(&mut self, shared: &Shared) -> Result<(), String> {
        shared.notified.store(false, Ordering::Release);
        let _span = shared.trace.span("native_tile_apply", "native", 0);
        let mut m = shared.mailbox.lock().unwrap();
        self.bounds = m.bounds;
        if m.resize || (self.width, self.height) != (m.width, m.height) {
            self.width = m.width;
            self.height = m.height;
            self.pixels = vec![0; self.width as usize * self.height as usize * 4];
            m.resize = false;
        }
        let tiles = std::mem::take(&mut m.tiles);
        drop(m);
        for (_, tile) in tiles {
            if tile.bgra.len() != tile.width as usize * tile.height as usize * 4
                || tile.left as u32 + tile.width as u32 > self.width as u32
                || tile.top as u32 + tile.height as u32 > self.height as u32
            {
                return Err("Native framebuffer tile bounds".into());
            }
            for y in 0..tile.height as usize {
                let dst = ((tile.top as usize + y) * self.width as usize + tile.left as usize) * 4;
                let src = y * tile.width as usize * 4;
                let count = tile.width as usize * 4;
                self.pixels[dst..dst + count].copy_from_slice(&tile.bgra[src..src + count]);
            }
        }
        Ok(())
    }
    fn viewport(&self) -> Bounds {
        if self.width == 0 || self.height == 0 || self.bounds.width == 0 || self.bounds.height == 0
        {
            return Bounds::default();
        }
        let scale = (self.bounds.width as f64 / self.width as f64)
            .min(self.bounds.height as f64 / self.height as f64)
            .min(1.0);
        let w = (self.width as f64 * scale).round().max(1.0) as u32;
        let h = (self.height as f64 * scale).round().max(1.0) as u32;
        Bounds {
            x: ((self.bounds.width - w) / 2) as i32,
            y: ((self.bounds.height - h) / 2) as i32,
            width: w,
            height: h,
        }
    }
    fn point(&self, x: i32, y: i32) -> Option<(u16, u16)> {
        let v = self.viewport();
        if v.width == 0
            || v.height == 0
            || x < v.x
            || y < v.y
            || x >= v.x + v.width as i32
            || y >= v.y + v.height as i32
        {
            return None;
        }
        Some((
            ((x - v.x) as u32 * self.width as u32 / v.width) as u16,
            ((y - v.y) as u32 * self.height as u32 / v.height) as u16,
        ))
    }
}
struct Keys {
    held: BTreeSet<(u16, bool)>,
    input: SyncSender<Input>,
    buttons: u16,
    position: (u16, u16),
}
impl Keys {
    fn new(input: SyncSender<Input>) -> Self {
        Self {
            held: BTreeSet::new(),
            input,
            buttons: 0,
            position: (0, 0),
        }
    }
    fn key(&mut self, code: u16, extended: bool, released: bool) {
        if released {
            self.held.remove(&(code, extended));
        } else {
            self.held.insert((code, extended));
        }
        let _ = self.input.send(Input::Scancode {
            code,
            extended,
            released,
        });
    }
    fn release(&mut self) {
        for (code, extended) in std::mem::take(&mut self.held) {
            let _ = self.input.send(Input::Scancode {
                code,
                extended,
                released: true,
            });
        }
        for flag in [0x1000, 0x2000, 0x4000] {
            if self.buttons & flag != 0 {
                let _ = self.input.send(Input::Mouse {
                    x: self.position.0,
                    y: self.position.1,
                    flags: flag,
                });
            }
        }
        self.buttons = 0;
    }
    fn mouse(&mut self, buffer: &Buffer, x: i32, y: i32, flags: u16) {
        let point = buffer.point(x, y).or({
            if flags & 0x7000 != 0 && flags & 0x8000 == 0 {
                Some(self.position)
            } else {
                None
            }
        });
        if let Some((x, y)) = point {
            self.position = (x, y);
            let button = flags & 0x7000;
            if flags & 0x8000 != 0 {
                self.buttons |= button;
            } else {
                self.buttons &= !button;
            }
            let input = Input::Mouse { x, y, flags };
            if flags == 0x800 {
                let _ = self.input.try_send(input);
            } else {
                let _ = self.input.send(input);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            error: Mutex::new(None),
            stop: AtomicBool::new(false),
            captured: AtomicBool::new(false),
            first_presented: AtomicBool::new(false),
            notified: AtomicBool::new(false),
            wake: Mutex::new(None),
            trace: Arc::new(rumstsc_rdp::trace::Trace::new(false)),
        })
    }
    fn snapshot(generation: u64, revision: u64, left: u16, color: u8) -> Snapshot {
        Snapshot {
            generation,
            revision,
            width: 2,
            height: 1,
            tiles: vec![TilePixels {
                index: left as usize,
                left,
                top: 0,
                width: 1,
                height: 1,
                bgra: vec![color, 0, 0, 255],
            }],
        }
    }
    #[test]
    fn skipped_presentations_merge_every_changed_tile() {
        let state = shared();
        let display = Display {
            handle: Handle(state.clone()),
        };
        display.submit(snapshot(1, 1, 0, 7));
        display.submit(snapshot(1, 2, 1, 9));
        let mut buffer = Buffer::default();
        buffer.receive(&state).unwrap();
        assert_eq!(buffer.pixels, [7, 0, 0, 255, 9, 0, 0, 255]);
        display.submit(snapshot(1, 3, 0, 11));
        display.submit(snapshot(2, 1, 1, 22));
        buffer.receive(&state).unwrap();
        assert_eq!(buffer.pixels, [0, 0, 0, 0, 22, 0, 0, 255]);
        display.handle().bounds(Bounds {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });
        buffer.receive(&state).unwrap();
        assert_eq!(buffer.point(3, 3), Some((0, 0)));
        assert_eq!(buffer.point(4, 3), Some((1, 0)));
        assert_eq!(buffer.point(0, 0), None);
    }
    #[test]
    fn native_focus_loss_releases_keys_and_outside_button_release() {
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let mut keys = Keys::new(tx);
        let buffer = Buffer {
            width: 2,
            height: 1,
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            ..Default::default()
        };
        keys.key(0x1e, false, false);
        keys.key(0x1d, true, false);
        keys.mouse(&buffer, 0, 0, 0x9000);
        keys.mouse(&buffer, -10, -10, 0x1000);
        keys.release();
        let inputs: Vec<_> = rx.try_iter().collect();
        assert_eq!(inputs.len(), 6);
        assert!(matches!(inputs[3], Input::Mouse { flags: 0x1000, .. }));
        assert!(
            inputs[4..]
                .iter()
                .all(|i| matches!(i, Input::Scancode { released: true, .. }))
        );
    }
}
