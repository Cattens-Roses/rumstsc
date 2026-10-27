//! Ordered, bounded graphics decoding and snapshots outside the network/UI threads.
use crate::{
    Error, Result,
    bitmap::{Frame, Snapshot},
    trace::Trace,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};
enum Command {
    Bitmap(Vec<u8>, Option<Instant>),
    Orders(Vec<u8>),
    Gfx(u64, Vec<u8>),
    GfxReset,
    Resize(u16, u16),
    Snapshot(u64, u64),
    Finish,
}
pub struct SnapshotClient {
    requests: SyncSender<Command>,
    pending: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
    changes: Arc<AtomicU64>,
    requested: AtomicU64,
    requested_generation: AtomicU64,
    requested_revision: AtomicU64,
    replies: Receiver<Snapshot>,
}
impl SnapshotClient {
    pub fn request(&self, generation: u64, revision: u64) {
        let changes = self.changes.load(Ordering::Acquire);
        if changes == self.requested.load(Ordering::Relaxed)
            && generation == self.requested_generation.load(Ordering::Relaxed)
            && revision == self.requested_revision.load(Ordering::Relaxed)
        {
            return;
        }
        if self.ready.load(Ordering::Acquire) || self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self
            .requests
            .try_send(Command::Snapshot(generation, revision))
            .is_err()
        {
            self.pending.store(false, Ordering::Release);
        } else {
            self.requested.store(changes, Ordering::Relaxed);
            self.requested_generation
                .store(generation, Ordering::Relaxed);
            self.requested_revision.store(revision, Ordering::Relaxed);
        }
    }
    pub fn take(&self) -> Option<Snapshot> {
        let snapshot = self.replies.try_recv().ok();
        if snapshot.is_some() {
            self.ready.store(false, Ordering::Release);
        }
        snapshot
    }
}
pub(crate) struct GfxReply {
    pub generation: u64,
    pub data: Vec<u8>,
    pub avc: bool,
}
pub(crate) struct Decoder {
    commands: Option<SyncSender<Command>>,
    errors: Receiver<Error>,
    gfx_replies: Receiver<GfxReply>,
    worker: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<thread::Thread>>>,
    pub trace: Arc<Trace>,
}
impl Decoder {
    pub fn start(
        frame: Arc<Mutex<Frame>>,
        stop: Arc<AtomicBool>,
        trace: Arc<Trace>,
    ) -> Result<(Self, SnapshotClient)> {
        let (tx, rx) = mpsc::sync_channel(8);
        let (gfx_tx, gfx_replies) = mpsc::sync_channel(64);
        let requests = tx.clone();
        let changes = Arc::new(AtomicU64::new(0));
        let worker_changes = changes.clone();
        let pending = Arc::new(AtomicBool::new(false));
        let worker_pending = pending.clone();
        let ready = Arc::new(AtomicBool::new(false));
        let worker_ready = ready.clone();
        let (reply_tx, replies) = mpsc::sync_channel(1);
        let (error_tx, errors) = mpsc::sync_channel(1);
        let worker_stop = stop.clone();
        let worker_trace = trace.clone();
        let waker: Arc<Mutex<Option<thread::Thread>>> = Arc::new(Mutex::new(None));
        let worker_waker = waker.clone();
        let worker = thread::Builder::new()
            .name("rdp-graphics".into())
            .spawn(move || {
                let mut orders = crate::orders::Orders::default();
                let mut gfx = crate::gfx::Gfx::default();
                let publish = |frame: &Frame, cursor: (u64, u64)| {
                    // Do not copy pixels for a reply the UI has not consumed.
                    if worker_ready.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    let _span = worker_trace.span("snapshot_copy", "graphics", cursor.1);
                    if let Some(snapshot) = frame.snapshot_since(cursor.0, cursor.1)
                        && reply_tx.try_send(snapshot).is_ok()
                    {
                        return;
                    }
                    worker_ready.store(false, Ordering::Release);
                };
                let mut process = |command, cursor: (u64, u64)| -> Result<()> {
                    match command {
                        Command::GfxReset => {
                            gfx = crate::gfx::Gfx::default();
                            frame.lock().unwrap().reset_frame_marker();
                        }
                        Command::Gfx(generation, bytes) => {
                            let _span =
                                worker_trace.span("rdpgfx_decode", "graphics", bytes.len() as u64);
                            let mut frame = frame.lock().unwrap();
                            let before = gfx.completed;
                            let codecs = gfx.counters;
                            let progressive = gfx.progressive_updates;
                            for reply in
                                gfx.update(&bytes, &mut frame, |frame| publish(frame, cursor))?
                            {
                                gfx_tx
                                    .try_send(GfxReply {
                                        data: reply,
                                        generation,
                                        avc: gfx.counters[0] > 0,
                                    })
                                    .map_err(|_| {
                                        crate::invalid("GFX reply queue full or disconnected")
                                    })?;
                            }
                            for (i, name) in [
                                "avc420_updates",
                                "clearcodec_updates",
                                "planar_updates",
                                "gfx_raw_updates",
                            ]
                            .iter()
                            .enumerate()
                            {
                                worker_trace.mark(name, "graphics", gfx.counters[i] - codecs[i]);
                            }
                            worker_trace.mark(
                                "progressive_updates",
                                "graphics",
                                gfx.progressive_updates - progressive,
                            );
                            worker_trace.mark("rdpgfx_frames", "graphics", gfx.completed - before);
                        }
                        Command::Bitmap(bytes, queued) => {
                            if let Some(queued) = queued {
                                worker_trace.mark(
                                    "bitmap_queue_age_us",
                                    "graphics",
                                    queued.elapsed().as_micros() as u64,
                                );
                            }
                            let _span =
                                worker_trace.span("bitmap_decode", "graphics", bytes.len() as u64);
                            let mut frame = frame.lock().unwrap();
                            let raw = frame.raw_rectangles;
                            let compressed = frame.compressed_rectangles;
                            frame.update(&bytes)?;
                            worker_trace.mark(
                                "raw_rectangles",
                                "graphics",
                                frame.raw_rectangles - raw,
                            );
                            worker_trace.mark(
                                "rle_rectangles",
                                "graphics",
                                frame.compressed_rectangles - compressed,
                            );
                            if frame.revision == 1 {
                                worker_trace.mark("first_bitmap_decoded", "graphics", 1);
                            }
                        }
                        Command::Orders(bytes) => {
                            let _span = worker_trace.span(
                                "screen_orders_decode",
                                "graphics",
                                bytes.len() as u64,
                            );
                            let mut frame = frame.lock().unwrap();
                            let before = frame.screen_blits;
                            let completed = frame.logical_frames;
                            orders.update_with_commit(&bytes, &mut frame, |frame| {
                                // Capture the complete frame before a following
                                // START in this same PDU can modify its pixels.
                                publish(frame, cursor);
                            })?;
                            if frame.logical_frames != completed {
                                worker_trace.mark(
                                    "logical_frames",
                                    "graphics",
                                    frame.logical_frames - completed,
                                );
                            }
                            worker_trace.mark(
                                "screen_blits",
                                "graphics",
                                frame.screen_blits - before,
                            );
                        }
                        Command::Snapshot(_, _) | Command::Finish => unreachable!(),
                        Command::Resize(w, h) => {
                            orders.reactivate();
                            let _span = worker_trace.span(
                                "framebuffer_resize",
                                "graphics",
                                w as u64 * h as u64,
                            );
                            let mut frame = frame.lock().unwrap();
                            let incomplete = frame.reset_frame_marker();
                            if incomplete || (frame.width, frame.height) != (w, h) {
                                *frame = Frame::new(w, h)?;
                            }
                        }
                    }
                    Ok(())
                };
                let mut cursor = (0, 0);
                loop {
                    if worker_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(command) = rx.recv() else {
                        break;
                    };
                    if let Some(sender) = worker_waker.lock().unwrap().as_ref() {
                        sender.unpark();
                    }
                    match command {
                        Command::Finish => break,
                        Command::Snapshot(generation, revision) => {
                            cursor = (generation, revision);
                            publish(&frame.lock().unwrap(), cursor);
                            worker_pending.store(false, Ordering::Release);
                        }
                        command => {
                            if let Err(error) = process(command, cursor) {
                                let _ = error_tx.send(error);
                                break;
                            }
                            worker_changes.fetch_add(1, Ordering::Release);
                        }
                    }
                }
                worker_trace.mark("graphics_stopped", "graphics", 0);
                if let Some(sender) = worker_waker.lock().unwrap().as_ref() {
                    sender.unpark();
                }
            })?;
        Ok((
            Self {
                commands: Some(tx),
                errors,
                gfx_replies,
                worker: Some(worker),
                stop,
                waker,
                trace,
            },
            SnapshotClient {
                requests,
                replies,
                pending,
                ready,
                changes,
                requested: AtomicU64::new(0),
                requested_generation: AtomicU64::new(0),
                requested_revision: AtomicU64::new(0),
            },
        ))
    }
    fn submit(&self, mut command: Command) -> Result<()> {
        let _span = self.trace.span("graphics_enqueue", "network", 0);
        *self.waker.lock().unwrap() = Some(thread::current());
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            self.check()?;
            match self.commands.as_ref().unwrap().try_send(command) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(pending)) => {
                    command = pending;
                    // A queue pop wakes this producer immediately. The timeout
                    // only bounds cancellation if decoding is still in progress.
                    thread::park_timeout(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(crate::invalid("graphics worker stopped"));
                }
            }
        }
    }
    pub fn gfx(&self, bytes: &[u8], generation: u64) -> Result<()> {
        self.submit(Command::Gfx(generation, bytes.to_vec()))
    }
    pub fn gfx_reset(&self) -> Result<()> {
        self.submit(Command::GfxReset)
    }
    pub fn gfx_reply(&self) -> Option<GfxReply> {
        self.gfx_replies.try_recv().ok()
    }
    pub fn bitmap(&self, bytes: &[u8]) -> Result<()> {
        self.submit(Command::Bitmap(
            bytes.to_vec(),
            self.trace.enabled().then(Instant::now),
        ))
    }
    pub fn orders(&self, bytes: &[u8]) -> Result<()> {
        self.submit(Command::Orders(bytes.to_vec()))
    }
    pub fn resize(&self, w: u16, h: u16) -> Result<()> {
        self.submit(Command::Resize(w, h))
    }
    pub fn check(&self) -> Result<()> {
        match self.errors.try_recv() {
            Ok(e) => Err(e),
            Err(_) => Ok(()),
        }
    }
    pub fn finish(&mut self) -> Result<()> {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::Finish);
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| crate::invalid("graphics worker panicked"))?;
        }
        self.check()
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn bitmap(x: u16, color: u8) -> Vec<u8> {
        let mut p = vec![1, 0];
        for n in [x, 0, x, 0, 1, 1, 24, 0, 4] {
            p.extend(n.to_le_bytes());
        }
        p.extend([color, 0, 0, 0]);
        p
    }
    #[test]
    fn unchanged_frames_do_not_wake_decoder() {
        let (requests, receiver) = mpsc::sync_channel(8);
        let (_, replies) = mpsc::sync_channel(1);
        let changes = Arc::new(AtomicU64::new(0));
        let client = SnapshotClient {
            requests,
            replies,
            changes: changes.clone(),
            requested: AtomicU64::new(0),
            requested_generation: AtomicU64::new(0),
            requested_revision: AtomicU64::new(0),
            pending: Arc::new(AtomicBool::new(false)),
            ready: Arc::new(AtomicBool::new(false)),
        };
        for _ in 0..100 {
            client.request(0, 0);
        }
        assert!(receiver.try_recv().is_err());
        changes.store(1, Ordering::Release);
        client.request(0, 0);
        assert!(matches!(receiver.try_recv(), Ok(Command::Snapshot(0, 0))));
        client.pending.store(false, Ordering::Release);
        for _ in 0..100 {
            client.request(0, 0);
        }
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn decoder_does_not_block_producer_on_frame_lock() {
        let frame = Arc::new(Mutex::new(Frame::new(2, 1).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let (mut decoder, _) =
            Decoder::start(frame.clone(), stop, Arc::new(Trace::default())).unwrap();
        let guard = frame.lock().unwrap();
        // Decoding cannot acquire the framebuffer yet, but the network-side
        // producer must still be able to enqueue its first update.
        decoder.bitmap(&bitmap(0, 7)).unwrap();
        decoder.bitmap(&bitmap(1, 9)).unwrap();
        drop(guard);
        decoder.finish().unwrap();
        let f = frame.lock().unwrap();
        assert_eq!(f.revision, 2);
        assert_eq!(&f.bgra[..4], &[7, 0, 0, 255]);
        assert_eq!(&f.bgra[4..], &[9, 0, 0, 255]);
    }
    #[test]
    fn resize_is_ordered_and_snapshots_run_off_ui_thread() {
        let frame = Arc::new(Mutex::new(Frame::new(1, 1).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let (mut decoder, client) =
            Decoder::start(frame.clone(), stop, Arc::new(Trace::default())).unwrap();
        decoder.resize(2, 1).unwrap();
        decoder.bitmap(&bitmap(1, 42)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            client.request(0, 0);
            if let Some(snapshot) = client.take() {
                assert_eq!((snapshot.width, snapshot.height), (2, 1));
                assert_eq!(&snapshot.tiles[0].bgra[4..], &[42, 0, 0, 255]);
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        decoder.finish().unwrap();
    }
    #[test]
    fn async_decode_errors_are_reported_and_stop_cancels_production() {
        let frame = Arc::new(Mutex::new(Frame::new(1, 1).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let (mut decoder, _) =
            Decoder::start(frame, stop.clone(), Arc::new(Trace::default())).unwrap();
        decoder.bitmap(&[1, 0]).unwrap();
        assert!(decoder.finish().is_err());
        stop.store(true, Ordering::Relaxed);
        assert!(matches!(
            decoder.bitmap(&bitmap(0, 3)),
            Err(Error::Cancelled)
        ));
    }
}
