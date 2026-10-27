//! Basic RDP sessions over authenticated TLS/CredSSP or pinned Standard RDP.
use crate::{
    Error, Result,
    bitmap::Frame,
    graphics::{Decoder, SnapshotClient},
    invalid,
    legacy::Legacy,
    mcs,
    negotiation::{self, Endpoint, Security, SecurityMode},
    trace::Trace,
    transport::{CertificateReview, ReviewCertificate, Transport},
    wire::{self, Reader, u16le, u32le},
};
use std::{
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Options {
    pub endpoint: Endpoint,
    pub username: String,
    pub domain: String,
    pub password: zeroize::Zeroizing<String>,
    pub security_mode: SecurityMode,
    pub certificate_pin: Option<[u8; 32]>,
    pub width: u16,
    pub height: u16,
    /// Explicit acknowledgement that this experimental subset sends plaintext.
    pub allow_insecure: bool,
}
#[derive(Debug)]
pub enum Event {
    Status(String),
    Security(String),
    Graphics(&'static str),
    Pointer(crate::pointer::Pointer),
    CertificateReview(CertificateReview),
    Active,
    Error(String),
    Closed,
}
#[derive(Clone, Copy, Debug)]
pub enum Input {
    Scancode {
        code: u16,
        extended: bool,
        released: bool,
    },
    Mouse {
        x: u16,
        y: u16,
        flags: u16,
    },
    Synchronize,
}
pub struct Connection {
    pub events: Receiver<Event>,
    pub frame: Arc<Mutex<Frame>>,
    pub trace: Arc<Trace>,
    snapshots: SnapshotClient,
    input: SyncSender<Input>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    certificate_decision: Option<SyncSender<String>>,
}
impl Connection {
    pub fn start(options: Options) -> Result<Self> {
        Self::start_with_review(options, false, None)
    }
    /// Enables certificate review events; the caller must approve or disconnect.
    /// Headless clients retain strict system trust through `start`.
    pub fn start_interactive(options: Options) -> Result<Self> {
        Self::start_with_review(options, true, None)
    }
    pub fn start_traced(options: Options, path: Option<std::path::PathBuf>) -> Result<Self> {
        Self::start_with_review(options, false, path)
    }
    pub fn start_interactive_traced(
        options: Options,
        path: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        Self::start_with_review(options, true, path)
    }
    fn start_with_review(
        options: Options,
        interactive: bool,
        trace_path: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        let trace = Arc::new(Trace::new(trace_path.is_some()));
        if let Some(path) = &trace_path {
            Trace::autosave(&trace, path.clone());
        }
        trace.mark("connection_start", "ui", 0);
        let frame = {
            let _span = trace.span(
                "framebuffer_allocate",
                "ui",
                options.width as u64 * options.height as u64,
            );
            Arc::new(Mutex::new(Frame::new(options.width, options.height)?))
        };
        let (tx, events) = mpsc::sync_channel(32);
        let (input, rx) = mpsc::sync_channel(256);
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let (mut graphics, snapshots) = Decoder::start(frame.clone(), stop.clone(), trace.clone())?;
        let (decision_tx, decision_rx) = mpsc::sync_channel(1);
        let review: Option<ReviewCertificate> = if interactive {
            let tx = tx.clone();
            let stop = stop.clone();
            let decisions = Mutex::new(decision_rx);
            let trace = trace.clone();
            Some(Arc::new(move |certificate: CertificateReview| {
                let _span = trace.span("certificate_review_wait", "network", 0);
                let fingerprint = certificate.fingerprint.clone();
                if tx.send(Event::CertificateReview(certificate)).is_err() {
                    return false;
                }
                let decisions = decisions.lock().unwrap();
                while !stop.load(Ordering::Relaxed) {
                    match decisions.recv_timeout(Duration::from_millis(100)) {
                        Ok(approved) => return approved == fingerprint,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return false,
                    }
                }
                false
            }))
        } else {
            None
        };
        let (s, c) = (stop.clone(), socket.clone());
        let worker_trace = trace.clone();
        thread::Builder::new()
            .name("rdp-session".into())
            .spawn(move || {
                let result = run(options, &tx, &mut graphics, rx, &s, &c, review);
                let graphics_result = graphics.finish();
                let result = result.and(graphics_result);
                worker_trace.mark("session_finished", "network", 0);
                if let Some(path) = trace_path {
                    let _ = worker_trace.save(&path);
                }
                if !s.load(Ordering::Relaxed)
                    && let Err(e) = result
                {
                    let _ = tx.send(Event::Error(e.to_string()));
                }
                if let Some(socket) = c.lock().unwrap().take() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                let _ = tx.send(Event::Closed);
            })?;
        Ok(Self {
            events,
            frame,
            trace,
            snapshots,
            input,
            stop,
            socket,
            certificate_decision: interactive.then_some(decision_tx),
        })
    }
    pub fn input_sender(&self) -> SyncSender<Input> {
        self.input.clone()
    }
    pub fn take_snapshot(&self) -> Option<crate::bitmap::Snapshot> {
        self.snapshots.take()
    }
    pub fn request_snapshot(&self, generation: u64, revision: u64) {
        self.snapshots.request(generation, revision);
    }
    /// Approves exactly the fingerprint shown by the current review event.
    pub fn trust_certificate_once(&self, fingerprint: &str) {
        if let Some(decision) = &self.certificate_decision {
            let _ = decision.try_send(fingerprint.into());
        }
    }
    /// Input is bounded; an overflow terminates the connection instead of losing
    /// a key release and leaving a modifier held down on the remote machine.
    pub fn send(&self, input: Input) {
        if self.input.try_send(input).is_err() {
            self.disconnect();
        }
    }
    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.disconnect();
    }
}
fn status(tx: &SyncSender<Event>, text: &str) {
    let _ = tx.try_send(Event::Status(text.into()));
}
fn send(stream: &mut Transport, user: u16, channel: u16, data: &[u8]) -> Result<()> {
    wire::write_data(stream, &mcs::send_data(user, channel, data)?)
}
fn share(kind: u16, user: u16, body: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    u16le(&mut out, (body.len() + 6) as u16);
    u16le(&mut out, kind | 0x10);
    u16le(&mut out, user);
    out.extend(body);
    out
}
pub fn data_pdu(user: u16, share_id: u32, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    u32le(&mut out, share_id);
    out.extend([0, 1]);
    u16le(&mut out, (body.len() + 18) as u16);
    out.extend([kind, 0]);
    u16le(&mut out, 0);
    out.extend(body);
    share(7, user, &out)
}
pub fn input_pdu(user: u16, share_id: u32, input: Input) -> Vec<u8> {
    let mut body = vec![1, 0, 0, 0];
    u32le(&mut body, 0);
    match input {
        Input::Scancode {
            code,
            extended,
            released,
        } => {
            u16le(&mut body, 4);
            u16le(
                &mut body,
                if extended { 0x100 } else { 0 } | if released { 0x8000 } else { 0 },
            );
            u16le(&mut body, code);
            u16le(&mut body, 0);
        }
        Input::Mouse { x, y, flags } => {
            u16le(&mut body, 0x8001);
            u16le(&mut body, flags);
            u16le(&mut body, x);
            u16le(&mut body, y);
        }
        Input::Synchronize => {
            u16le(&mut body, 0);
            body.extend([0; 6]);
        }
    }
    data_pdu(user, share_id, 28, &body)
}
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
fn client_info(o: &Options) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let strings = [
        utf16(&o.domain),
        utf16(&o.username),
        utf16(&o.password),
        vec![],
        vec![],
    ]
    .map(zeroize::Zeroizing::new);
    if strings.iter().any(|s| s.len() > 512) {
        return Err(invalid("username/domain exceeds 256 UTF-16 units"));
    }
    let mut out = zeroize::Zeroizing::new(vec![0x40, 0, 0, 0]);
    u32le(&mut out, 0);
    u32le(
        &mut out,
        0x000103f3 | if o.password.is_empty() { 0 } else { 8 },
    );
    for s in &strings {
        u16le(&mut out, s.len() as u16);
    }
    for s in strings {
        out.extend(s.iter().copied());
        out.extend([0, 0]);
    }
    // TS_EXTENDED_INFO_PACKET: address, client directory, UTC timezone, performance.
    u16le(&mut out, 2);
    let address = utf16("0.0.0.0\0");
    u16le(&mut out, address.len() as u16);
    out.extend(address);
    let directory = utf16("C:\\rumstsc\0");
    u16le(&mut out, directory.len() as u16);
    out.extend(directory);
    out.extend([0; 172]);
    u32le(&mut out, 0);
    u32le(&mut out, 0x0000000f);
    u16le(&mut out, 0);
    Ok(out)
}
fn cap(out: &mut Vec<u8>, kind: u16, bytes: &[u8]) {
    u16le(out, kind);
    u16le(out, (bytes.len() + 4) as u16);
    out.extend(bytes);
}
#[cfg(test)]
fn confirm_active(user: u16, share_id: u32, width: u16, height: u16) -> Vec<u8> {
    confirm_active_depth(user, share_id, width, height, 24)
}
fn confirm_active_depth(user: u16, share_id: u32, width: u16, height: u16, depth: u16) -> Vec<u8> {
    let mut caps = vec![];
    let mut general = vec![];
    for v in [4, 7, 0x200, 0, 0, 0, 0, 0, 0] {
        u16le(&mut general, v);
    }
    general.extend([0, 0]);
    cap(&mut caps, 1, &general);
    let mut bitmap = vec![];
    for v in [depth, 1, 1, 1, width, height, 0, 1, 1] {
        u16le(&mut bitmap, v);
    }
    bitmap.extend([0, 0]);
    u16le(&mut bitmap, 1);
    u16le(&mut bitmap, 0);
    cap(&mut caps, 2, &bitmap);
    let mut orders = vec![0; 84];
    orders[20..22].copy_from_slice(&1u16.to_le_bytes());
    orders[22..24].copy_from_slice(&20u16.to_le_bytes());
    orders[26..28].copy_from_slice(&1u16.to_le_bytes()); // maximumOrderLevel
    orders[30..32].copy_from_slice(&0x008au16.to_le_bytes());
    orders[32 + 3] = 1; // MemBlt from the bounded session bitmap cache.
    orders[32 + 2] = 1; // TS_NEG_SCRBLT_INDEX: copy existing screen pixels.
    orders[66..68].copy_from_slice(&4u16.to_le_bytes()); // Alternate frame markers.
    cap(&mut caps, 3, &orders); // ScrBlt and logical frame markers.
    let mut bitmap_cache = vec![0; 24];
    for i in 0..3 {
        u16le(&mut bitmap_cache, crate::cache::ENTRIES[i]);
        u16le(&mut bitmap_cache, crate::cache::CELL_BYTES[i]);
    }
    cap(&mut caps, 4, &bitmap_cache);
    cap(&mut caps, 5, &[0; 8]);
    cap(&mut caps, 7, &[0; 8]);
    cap(&mut caps, 8, &[1, 0, 25, 0, 25, 0]); // Color and arbitrary-depth pointer caches.
    cap(&mut caps, 9, &[0; 4]);
    cap(&mut caps, 10, &[0; 4]);
    let mut input = vec![0; 84];
    input[0..2].copy_from_slice(&1u16.to_le_bytes());
    input[4..8].copy_from_slice(&0x409u32.to_le_bytes());
    input[8..12].copy_from_slice(&4u32.to_le_bytes());
    input[16..20].copy_from_slice(&12u32.to_le_bytes());
    cap(&mut caps, 13, &input);
    cap(&mut caps, 14, &[1, 0, 0, 0]);
    let mut virtual_channel = vec![];
    u32le(&mut virtual_channel, 0); // VCCAPS_NO_COMPR; GFX has its own compression.
    u32le(&mut virtual_channel, 1600);
    cap(&mut caps, 20, &virtual_channel);
    let mut body = vec![];
    u32le(&mut body, share_id);
    u16le(&mut body, 1002);
    u16le(&mut body, 8);
    u16le(&mut body, (caps.len() + 4) as u16);
    body.extend(b"rumstsc\0");
    u16le(&mut body, 12);
    u16le(&mut body, 0);
    body.extend(caps);
    share(3, user, &body)
}
fn demand_active(body: &[u8], desktop: &mut (u16, u16)) -> Result<u32> {
    let mut r = Reader::new(body);
    let share_id = r.u32()?;
    let source_len = r.u16()? as usize;
    let caps_len = r.u16()? as usize;
    r.take(source_len)?;
    let mut caps = Reader::new(r.take(caps_len)?);
    let count = caps.u16()?;
    caps.u16()?;
    for _ in 0..count {
        let kind = caps.u16()?;
        let len = caps.u16()? as usize;
        if len < 4 {
            return Err(invalid("capability length"));
        }
        let mut c = Reader::new(caps.take(len - 4)?);
        if kind == 2 {
            c.take(8)?;
            let width = c.u16()?;
            let height = c.u16()?;
            Frame::validate_dimensions(width, height)?;
            *desktop = (width, height);
        }
    }
    if caps.remaining() != 0 {
        return Err(invalid("combined capability length"));
    }
    // sessionId follows capabilities in Demand Active.
    r.u32()?;
    if r.remaining() != 0 {
        return Err(invalid("trailing Demand Active data"));
    }
    Ok(share_id)
}
fn run(
    mut o: Options,
    tx: &SyncSender<Event>,
    graphics: &mut Decoder,
    input: Receiver<Input>,
    stop: &AtomicBool,
    socket: &Mutex<Option<TcpStream>>,
    review: Option<ReviewCertificate>,
) -> Result<()> {
    let trace = graphics.trace.clone();
    let mut desktop = (o.width, o.height);
    if o.domain.is_empty()
        && let Some((domain, user)) = o.username.split_once('\\')
    {
        o.domain = domain.into();
        o.username = user.into();
    }
    status(tx, "Resolving host and opening TCP…");
    let mut stream = {
        let _span = trace.span("dns_tcp_connect", "network", 0);
        o.endpoint.connect_traced(&trace)?
    };
    *socket.lock().unwrap() = Some(stream.try_clone()?);
    if stop.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    status(tx, "Negotiating RDP security…");
    let (security, server_flags) = {
        let _span = trace.span("rdp_negotiation", "network", 0);
        negotiation::negotiate_mode_flags(&mut stream, o.security_mode)?
    };
    let mut stream = if security == Security::Legacy {
        Transport::Plain(stream)
    } else {
        status(tx, "Verifying TLS server identity…");
        let _span = trace.span("tls_setup_and_handshake", "network", 0);
        Transport::tls_with_review_traced(
            stream,
            &o.endpoint.host,
            o.certificate_pin,
            review,
            &trace,
        )?
    };
    if security == Security::Nla {
        status(tx, "Authenticating with NLA / CredSSP…");
        let _span = trace.span("nla_authentication", "network", 0);
        crate::credssp::authenticate(
            &mut stream,
            &o.username,
            &o.domain,
            &o.password,
            &o.endpoint.host,
        )?;
    }
    status(tx, "Exchanging desktop settings…");
    let selected = match security {
        Security::Legacy => 0,
        Security::Tls => 1,
        Security::Nla => 2,
        _ => unreachable!(),
    };
    let gfx_enabled = server_flags & 1 != 0
        && rumstsc_video::available()
        && std::env::var_os("RUMSTSC_GFX").is_none_or(|v| v != "0");
    let mcs_span = trace.span("mcs_settings_exchange", "network", 0);
    wire::write_data(
        &mut stream,
        &mcs::connect_initial_graphics(
            o.width,
            o.height,
            selected,
            security == Security::Legacy,
            gfx_enabled,
        )?,
    )?;
    let settings = mcs::connect_response(&wire::read_data(&mut stream)?)?;
    drop(mcs_span);
    if settings
        .requested_protocols
        .is_some_and(|p| p != o.security_mode.protocols())
    {
        return Err(Error::Security(
            "server echoed different RDP negotiation protocols".into(),
        ));
    }
    if security != Security::Legacy
        && (settings.encryption_method != 0 || settings.encryption_level != 0)
    {
        return Err(invalid("Standard RDP encryption selected inside TLS"));
    }
    let (mut protection, exchange) =
        if settings.encryption_method != 0 || settings.encryption_level != 0 {
            status(tx, "Establishing pinned legacy RDP encryption…");
            let (legacy, exchange) = Legacy::exchange(
                settings.encryption_method,
                settings.encryption_level,
                &settings.server_random,
                &settings.certificate,
                o.certificate_pin,
            )?;
            (Some(legacy), Some(exchange))
        } else {
            (None, None)
        };
    if security == Security::Legacy && protection.is_none() && !o.allow_insecure {
        return Err(Error::Unsupported("server selected plaintext RDP. Enable the lab-only plaintext option to continue; no credentials were sent".into()));
    }
    status(tx, "Joining RDP channels…");
    let channel_span = trace.span("channel_attach_and_joins", "network", 0);
    // These ordered MCS requests need no response between them. Send their
    // separate TPKTs in one TLS write, avoiding a tiny-record exchange that
    // showed an extra ~40 ms wait with the local TLS peer.
    let mut attach = Vec::new();
    wire::write_data(&mut attach, &[4, 1, 0, 1, 0])?;
    wire::write_data(&mut attach, &[0x28])?;
    stream.write_all(&attach)?;
    let user = mcs::attach_confirm(&wire::read_data(&mut stream)?)?;
    if settings.static_channels.len() > usize::from(gfx_enabled) {
        return Err(invalid("unrequested static channels"));
    }
    let gfx_channel = settings.static_channels.first().copied();
    let mut channels = vec![user, settings.channel];
    channels.extend(&settings.static_channels);
    if settings.message_channel.is_some() && !gfx_enabled {
        return Err(invalid("unrequested message channel"));
    }
    channels.extend(settings.message_channel);
    for channel in channels {
        let _span = trace.span("channel_join_roundtrip", "network", 0);
        wire::write_data(&mut stream, &mcs::join(user, channel))?;
        mcs::join_confirm(&wire::read_data(&mut stream)?, user, channel)?;
    }
    drop(channel_span);
    if stop.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if security == Security::Legacy && protection.is_none() && !o.password.is_empty() {
        return Err(Error::Security(
            "password submission over plaintext RDP is refused".into(),
        ));
    }
    if let Some(exchange) = exchange {
        send(&mut stream, user, settings.channel, &exchange)?;
    }
    status(tx, "Waiting for licensing and desktop capabilities…");
    trace.mark("client_info_sending", "network", 0);
    let info = client_info(&o)?;
    if let Some(legacy) = &mut protection {
        let encrypted = zeroize::Zeroizing::new(legacy.protect(0x40, &info[4..]));
        send(&mut stream, user, settings.channel, &encrypted)?;
    } else {
        send(&mut stream, user, settings.channel, &info)?;
    }
    drop(info);
    zeroize::Zeroize::zeroize(&mut *o.password);
    let _ = tx.send(Event::Security(
        match security {
            Security::Nla => "TLS + NLA",
            Security::Tls => "TLS",
            _ if protection.is_some() => "Legacy RC4",
            _ => "Plaintext lab session",
        }
        .into(),
    ));
    status(
        tx,
        match security {
            Security::Nla => "NLA authenticated · activating desktop…",
            Security::Tls => "TLS verified · activating desktop…",
            _ if protection.is_some() => "Legacy encryption · activating desktop…",
            _ => "Plaintext lab session · activating desktop…",
        },
    );
    stream.set_nonblocking(true)?;
    let mut bulk = crate::bulk::Bulk::default();
    let mut pointers = crate::pointer::Decoder::default();
    let mut dvc = crate::dvc::Dvc::default();
    let mut autodetect = crate::autodetect::AutoDetect::default();
    let mut graphics_label = "";
    let mut gfx_generation = 0u64;
    let mut pending = vec![];
    let mut writes = vec![];
    let mut written = 0;
    let mut share_id = None;
    let mut active = false;
    let mut last_packet = Instant::now();
    let mut licensing = crate::licensing::Licensing::default();
    let mut buf = [0; 65536];
    loop {
        graphics.check()?;
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if !active && last_packet.elapsed() > Duration::from_secs(15) {
            return Err(invalid("session activation timed out"));
        }
        let mut progressed = false;
        if writes.len() - written < 65536 && active {
            for _ in 0..64 {
                match input.try_recv() {
                    Ok(i) => queue(
                        &mut writes,
                        user,
                        settings.channel,
                        &input_pdu(user, share_id.unwrap(), i),
                        &mut protection,
                    )?,
                    Err(_) => break,
                }
            }
        }
        if let Some(channel) = gfx_channel {
            while let Some(reply) = graphics.gfx_reply() {
                if reply.generation != gfx_generation {
                    continue;
                }
                let label = if reply.avc {
                    "RDPGFX / AVC420"
                } else {
                    "RDPGFX"
                };
                if graphics_label != label {
                    graphics_label = label;
                    let _ = tx.send(Event::Graphics(label));
                }
                for packet in dvc.send(&reply.data)? {
                    queue_channel(&mut writes, user, channel, &packet, &mut protection)?;
                }
            }
        }
        if written < writes.len() {
            match stream.write(&writes[written..]) {
                Ok(0) => return Err(invalid("connection closed during send")),
                Ok(n) => {
                    written += n;
                    trace.mark("network_write", "network", n as u64);
                    progressed = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        if written == writes.len() {
            writes.clear();
            written = 0;
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                trace.mark("network_read", "network", n as u64);
                pending.extend(&buf[..n]);
                last_packet = Instant::now();
                progressed = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if pending.len() > 1_048_576 {
            return Err(invalid("receive buffer limit"));
        }
        let mut consumed = 0;
        while pending.len() - consumed >= 4 {
            let h = &pending[consumed..];
            if h[..2] != [3, 0] {
                return Err(Error::Unsupported(
                    "fast-path output (not advertised)".into(),
                ));
            }
            let n = u16::from_be_bytes([h[2], h[3]]) as usize;
            if n < 7 {
                return Err(invalid("short TPKT"));
            }
            if h.len() < n {
                break;
            }
            let _packet_span = trace.span("rdp_packet_process", "network", n as u64);
            let packet = &h[4..n];
            if packet[..3] != [2, 0xf0, 0x80] {
                return Err(invalid("X.224 data header"));
            }
            let (channel, payload) = mcs::receive_channel_data(&packet[3..])?;
            if channel != settings.channel
                && Some(channel) != gfx_channel
                && Some(channel) != settings.message_channel
            {
                return Err(invalid("unexpected MCS channel"));
            }
            let (flags, data) = if let Some(legacy) = &mut protection {
                legacy.unprotect(payload)?
            } else if Some(channel) == settings.message_channel {
                let mut message = Reader::new(payload);
                let flags = message.u16()?;
                message.u16()?;
                (flags, message.rest().to_vec())
            } else if channel == settings.channel
                && !licensing.complete
                && payload.len() >= 8
                && u16::from_le_bytes([payload[0], payload[1]]) & !0x0280 == 0
                && payload[0] & 0x80 != 0
                && u16::from_le_bytes([payload[6], payload[7]]) as usize == payload.len() - 4
            {
                (
                    u16::from_le_bytes([payload[0], payload[1]]),
                    payload[4..].to_vec(),
                )
            } else {
                (0, payload.to_vec())
            };
            autodetect.observe(data.len());
            trace.mark("mcs_channel_received", "network", channel as u64);
            if Some(channel) == settings.message_channel {
                if flags & !0x0808 != 0x1000 {
                    return Err(invalid("message channel security flags"));
                }
                trace.mark("autodetect_request", "network", 1);
                if let Some(reply) = autodetect.receive(&data)? {
                    let framed = if let Some(legacy) = &mut protection {
                        legacy.protect(0x2000, &reply)
                    } else {
                        let mut framed = vec![0, 0x20, 0, 0];
                        framed.extend(reply);
                        framed
                    };
                    queue(&mut writes, user, channel, &framed, &mut None)?;
                    trace.mark("autodetect_response", "network", 1);
                }
            } else if Some(channel) == gfx_channel {
                let mut virtual_data = Reader::new(&data);
                let total = virtual_data.u32()?;
                let channel_flags = virtual_data.u32()?;
                let decoded = bulk.decode((channel_flags >> 16) as u8, virtual_data.rest())?;
                let mut decoded_channel = vec![];
                u32le(&mut decoded_channel, total);
                u32le(&mut decoded_channel, channel_flags & 0xffff);
                decoded_channel.extend(decoded);
                if channel_flags & 1 != 0
                    && let Some(control) = decoded_channel.get(8)
                {
                    trace.mark("dvc_server_control", "network", *control as u64);
                    if control >> 4 == 5 && decoded_channel.len() >= 12 {
                        trace.mark(
                            "dvc_server_version",
                            "network",
                            u16::from_le_bytes([decoded_channel[10], decoded_channel[11]]) as u64,
                        );
                    }
                }
                for action in dvc.receive(&decoded_channel)? {
                    match action {
                        crate::dvc::Action::Reply(packet) => {
                            trace.mark("dvc_client_control", "network", packet[0] as u64);
                            queue_channel(&mut writes, user, channel, &packet, &mut protection)?
                        }
                        crate::dvc::Action::Opened => {
                            gfx_generation += 1;
                            graphics.gfx_reset()?;
                            for packet in dvc.send(&crate::gfx::capabilities())? {
                                queue_channel(
                                    &mut writes,
                                    user,
                                    channel,
                                    &packet,
                                    &mut protection,
                                )?;
                            }
                            trace.mark("rdpgfx_channel_opened", "network", 1);
                            trace.mark("rdpgfx_capabilities_sent", "network", 1);
                        }
                        crate::dvc::Action::Graphics(bytes) => {
                            graphics.gfx(&bytes, gfx_generation)?
                        }
                        crate::dvc::Action::Closed => {
                            gfx_generation += 1;
                            graphics.gfx_reset()?;
                            graphics_label = "";
                            let _ = tx.send(Event::Graphics("Bitmap"));
                        }
                    }
                }
            } else if flags & 0x80 != 0 {
                let tls_cert = if security != Security::Legacy {
                    Some(stream.certificate()?)
                } else {
                    None
                };
                if let Some(reply) =
                    licensing.process(&data, &o.username, &settings.certificate, tls_cert)?
                {
                    // License blobs have their own encryption/MAC. Keep their
                    // Basic Security Header even when outer transport is TLS.
                    let mut license = vec![];
                    u16le(&mut license, 0x80);
                    u16le(&mut license, 0);
                    license.extend(reply);
                    let mut packet = vec![2, 0xf0, 0x80];
                    packet.extend(mcs::send_data(user, settings.channel, &license)?);
                    writes.extend(wire::tpkt(&packet)?);
                }
            } else {
                let mut pdus = Reader::new(&data);
                while pdus.remaining() > 0 {
                    let total = pdus.u16()? as usize;
                    if total < 6 {
                        return Err(invalid("Share Control PDU length"));
                    }
                    let mut p = Reader::new(pdus.take(total - 2)?);
                    let kind = p.u16()?;
                    let server = p.u16()?;
                    match kind & 15 {
                        1 => {
                            active = false;
                            trace.mark("demand_active_received", "network", 0);
                            let id = demand_active(p.rest(), &mut desktop)?;
                            graphics.resize(desktop.0, desktop.1)?;
                            share_id = Some(id);
                            let (width, height) = desktop;
                            queue(
                                &mut writes,
                                user,
                                settings.channel,
                                &confirm_active_depth(
                                    user,
                                    id,
                                    width,
                                    height,
                                    if gfx_channel.is_some() { 32 } else { 24 },
                                ),
                                &mut protection,
                            )?;
                            let mut sync = vec![];
                            u16le(&mut sync, 1);
                            u16le(&mut sync, server);
                            queue(
                                &mut writes,
                                user,
                                settings.channel,
                                &data_pdu(user, id, 31, &sync),
                                &mut protection,
                            )?;
                            for action in [4, 1] {
                                let mut b = vec![];
                                u16le(&mut b, action);
                                u16le(&mut b, 0);
                                u32le(&mut b, 0);
                                queue(
                                    &mut writes,
                                    user,
                                    settings.channel,
                                    &data_pdu(user, id, 20, &b),
                                    &mut protection,
                                )?;
                            }
                            queue(
                                &mut writes,
                                user,
                                settings.channel,
                                &data_pdu(user, id, 39, &[0, 0, 0, 0, 3, 0, 0x32, 0]),
                                &mut protection,
                            )?;
                            status(tx, "Activating remote desktop…");
                        }
                        6 => {
                            active = false;
                            share_id = None;
                            status(
                                tx,
                                "Server deactivated the desktop; waiting for reactivation…",
                            );
                        }
                        7 => {
                            let id = p.u32()?;
                            if share_id != Some(id) {
                                return Err(invalid("Share ID mismatch"));
                            }
                            p.take(2)?;
                            let _uncompressed = p.u16()?;
                            let subtype = p.u8()?;
                            trace.mark("share_data_type", "network", subtype as u64);
                            let compression = p.u8()?;
                            let _compressed_len = p.u16()?;
                            let encoded = p.rest();
                            let decoded = bulk.decode(compression, encoded)?;
                            if compression & 0x20 != 0 {
                                trace.mark(
                                    "bulk_compressed_bytes",
                                    "network",
                                    encoded.len() as u64,
                                );
                                trace.mark("bulk_expanded_bytes", "network", decoded.len() as u64);
                            }
                            let mut p = Reader::new(decoded);
                            match subtype {
                                2 => {
                                    if !active {
                                        return Err(invalid("bitmap before Font Map"));
                                    }
                                    let update = p.u16()?;
                                    match update {
                                        0 => graphics.orders(p.rest())?,
                                        1 => graphics.bitmap(p.rest())?,
                                        3 => {}
                                        n => {
                                            return Err(Error::Unsupported(format!(
                                                "graphics update type {n}"
                                            )));
                                        }
                                    }
                                }
                                27 => {
                                    if let Some(pointer) = pointers.update(p.rest())? {
                                        trace.mark("pointer_update", "network", 1);
                                        let _ = tx.send(Event::Pointer(pointer));
                                    }
                                }
                                40 => {
                                    if !active {
                                        active = true;
                                        trace.mark("font_map_active", "network", 0);
                                        let _ = tx.send(Event::Active);
                                    }
                                }
                                47 => {
                                    let code = p.u32()?;
                                    if code != 0 {
                                        return Err(Error::Protocol(format!(
                                            "server error 0x{code:08x}"
                                        )));
                                    }
                                }
                                20 | 31 | 38 | 54 | 55 => {}
                                _ => {}
                            }
                        }
                        _ => {
                            return Err(Error::Unsupported(format!(
                                "Share Control type {}",
                                kind & 15
                            )));
                        }
                    }
                }
            }
            consumed += n;
        }
        if consumed > 0 {
            pending.drain(..consumed);
        }
        // Keep draining queued TCP/TLS data without introducing a delay per
        // record. Sleep only when neither direction made progress. Each pass
        // still checks cancellation and services keyboard/mouse input.
        if !progressed {
            thread::sleep(Duration::from_millis(2));
        }
    }
}
fn queue_channel(
    out: &mut Vec<u8>,
    user: u16,
    channel: u16,
    payload: &[u8],
    protection: &mut Option<Legacy>,
) -> Result<()> {
    // DVC packets are deliberately below the 1600-byte static-channel chunk.
    let mut framed = vec![];
    u32le(&mut framed, payload.len() as u32);
    u32le(&mut framed, 3);
    framed.extend(payload);
    queue(out, user, channel, &framed, protection)
}

fn queue(
    out: &mut Vec<u8>,
    user: u16,
    channel: u16,
    payload: &[u8],
    protection: &mut Option<Legacy>,
) -> Result<()> {
    if out.len() + payload.len() + 32 > 1_048_576 {
        return Err(invalid("send buffer limit"));
    }
    let mut data = vec![2, 0xf0, 0x80];
    let encrypted;
    let payload = if let Some(legacy) = protection {
        encrypted = legacy.protect(0, payload);
        encrypted.as_slice()
    } else {
        payload
    };
    data.extend(mcs::send_data(user, channel, payload)?);
    out.extend(wire::tpkt(&data)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_release_and_lengths() {
        let p = input_pdu(
            1005,
            0x10001,
            Input::Scancode {
                code: 0x1d,
                extended: true,
                released: true,
            },
        );
        assert_eq!(p.len(), 34);
        assert_eq!(u16::from_le_bytes([p[0], p[1]]), 34);
        assert_eq!(&p[28..32], &[0, 0x81, 0x1d, 0]);
    }
    #[test]
    fn capability_lengths() {
        let p = confirm_active(1005, 0x10001, 1280, 720);
        let mut r = Reader::new(&p);
        assert_eq!(r.u16().unwrap() as usize, p.len());
        r.take(10).unwrap();
        let source = r.u16().unwrap() as usize;
        let combined = r.u16().unwrap() as usize;
        r.take(source).unwrap();
        assert_eq!(combined, r.remaining());
        assert_eq!(r.u16().unwrap(), 12);
        r.u16().unwrap();
        for _ in 0..12 {
            let kind = r.u16().unwrap();
            let n = r.u16().unwrap() as usize;
            let capability = r.take(n - 4).unwrap();
            if kind == 8 {
                assert_eq!(capability, [1, 0, 25, 0, 25, 0]);
            }
            if kind == 20 {
                assert_eq!(capability, [0, 0, 0, 0, 0x40, 6, 0, 0]);
            }
            if kind == 3 {
                let mut order = Reader::new(capability);
                order.take(16).unwrap(); // terminalDescriptor
                order.u32().unwrap(); // pad4octetsA
                order.u16().unwrap(); // desktopSaveXGranularity
                order.u16().unwrap(); // desktopSaveYGranularity
                order.u16().unwrap(); // pad2octetsA
                assert_eq!(order.u16().unwrap(), 1); // maximumOrderLevel
                assert_eq!(order.u16().unwrap(), 0); // numberFonts
                assert_eq!(order.u16().unwrap(), 0x008a); // orderFlags
                let mut supported = [0u8; 32];
                supported[2] = 1;
                supported[3] = 1;
                assert_eq!(order.take(32).unwrap(), &supported);
                order.u16().unwrap(); // textFlags
                assert_eq!(order.u16().unwrap(), 4); // orderSupportExFlags
            }
        }
        assert_eq!(r.remaining(), 0);
    }
}
