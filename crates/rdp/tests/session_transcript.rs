//! Local protocol peer, not a real Windows/xrdp interoperability test.
//! Server PDUs are independently assembled here to catch framing/state mistakes.
use rumstsc_rdp::{
    negotiation::Endpoint,
    session::{Connection, Event, Input, Options},
    wire::{self, Reader},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};
fn le16(b: &mut Vec<u8>, n: u16) {
    b.extend(n.to_le_bytes());
}
fn le32(b: &mut Vec<u8>, n: u32) {
    b.extend(n.to_le_bytes());
}
fn accept(listener: TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "localhost connection timed out");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("localhost accept failed: {e}"),
        }
    }
}
fn server_settings_channels(encrypted: bool, graphics: bool, clipboard: bool) -> Vec<u8> {
    let mut blocks = vec![];
    for (kind, mut body) in [
        (0x0c01, 0x80004u32.to_le_bytes().to_vec()),
        (
            0x0c02,
            vec![
                if encrypted { 2 } else { 0 },
                0,
                0,
                0,
                if encrypted { 3 } else { 0 },
                0,
                0,
                0,
            ],
        ),
        (0x0c03, {
            let mut net = vec![0xeb, 3];
            let count = u16::from(graphics) + u16::from(clipboard);
            le16(&mut net, count);
            if graphics {
                le16(&mut net, 1006);
            }
            if clipboard {
                le16(&mut net, 1006 + u16::from(graphics));
            }
            if count % 2 != 0 {
                le16(&mut net, 0);
            }
            net
        }),
    ] {
        if kind == 0x0c02 && encrypted {
            le32(&mut body, 32);
            le32(&mut body, 4);
            body.extend([0x55; 32]);
            body.extend([1, 0, 0, 0]);
        }
        le16(&mut blocks, kind);
        le16(&mut blocks, (body.len() + 4) as u16);
        blocks.extend(body);
    }
    if graphics {
        le16(&mut blocks, 0x0c04);
        le16(&mut blocks, 6);
        le16(&mut blocks, 1007 + u16::from(clipboard));
    }
    let mut gcc = vec![
        0, 5, 0, 0x14, 0x7c, 0, 1, 0x2a, 0x14, 0x76, 0x0a, 1, 1, 0, 1, 0xc0, 0,
    ];
    gcc.extend(b"McDn");
    gcc.push(blocks.len() as u8);
    gcc.extend(blocks);
    let mut body = vec![0x0a, 1, 0, 2, 1, 0, 0x30, 0];
    body.extend(wire::ber(&[4], &gcc));
    wire::ber(&[0x7f, 0x66], &body)
}
fn setup(stream: &mut TcpStream, encrypted: bool) {
    setup_graphics(stream, encrypted, false);
}
fn setup_graphics(stream: &mut TcpStream, encrypted: bool, graphics: bool) -> bool {
    setup_channels(stream, encrypted, graphics, false)
}
fn setup_channels(
    stream: &mut TcpStream,
    encrypted: bool,
    graphics: bool,
    clipboard: bool,
) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let p = wire::read_tpkt(stream).unwrap();
    assert_eq!(p[1], 0xe0);
    let mut confirm = [3, 0, 0, 19, 14, 0xd0, 0, 0, 0, 0, 0, 2, 0, 8, 0, 0, 0, 0, 0];
    confirm[12] = u8::from(graphics); // EXTENDED_CLIENT_DATA_SUPPORTED
    stream.write_all(&confirm).unwrap();
    let p = wire::read_data(stream).unwrap();
    assert_eq!(&p[..2], &[0x7f, 0x65]);
    let requested_graphics = p.windows(8).any(|v| v == b"drdynvc\0");
    assert_eq!(p.windows(8).any(|v| v == b"cliprdr\0"), clipboard);
    if graphics {
        assert!(requested_graphics);
        assert!(
            p.windows(8).any(|v| v == [6, 192, 8, 0, 0, 0, 0, 0]),
            "missing CS_MCS_MSGCHANNEL"
        );
        let core = p.windows(4).position(|v| v == [1, 192, 216, 0]).unwrap() + 4;
        assert_eq!(&p[core + 140..core + 142], &0x182u16.to_le_bytes());
    }
    wire::write_data(
        stream,
        &server_settings_channels(encrypted, graphics, clipboard),
    )
    .unwrap();
    requested_graphics
}
fn options(listener: &TcpListener, insecure: bool) -> Options {
    Options {
        endpoint: Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap(),
        username: "tester".into(),
        domain: "lab".into(),
        password: String::new().into(),
        security_mode: rumstsc_rdp::negotiation::SecurityMode::Legacy,
        certificate_pin: None,
        width: 2,
        height: 2,
        allow_insecure: insecure,
        clipboard: false,
    }
}
fn indication(payload: &[u8]) -> Vec<u8> {
    let mut p = vec![2, 0xf0, 0x80, 0x68, 0, 1, 3, 0xeb, 0x70];
    wire::per_length(&mut p, payload.len()).unwrap();
    p.extend(payload);
    wire::tpkt(&p).unwrap()
}
fn share(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut p = vec![];
    le16(&mut p, (body.len() + 6) as u16);
    le16(&mut p, kind | 0x10);
    le16(&mut p, 1002);
    p.extend(body);
    p
}
fn data(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![];
    le32(&mut p, 0x10001);
    p.extend([0, 1]);
    le16(&mut p, (body.len() + 18) as u16);
    p.extend([kind, 0, 0, 0]);
    p.extend(body);
    share(7, &p)
}
fn client_payload(s: &mut TcpStream) -> Vec<u8> {
    let p = wire::read_data(s).unwrap();
    let mut r = Reader::new(&p);
    assert_eq!(r.u8().unwrap(), 0x64);
    r.take(5).unwrap();
    let n = r.per_length().unwrap();
    r.take(n).unwrap().to_vec()
}
#[test]
fn clipboard_channel_unicode_roundtrip_and_paste_barrier() {
    fn clip(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
        let mut p = vec![];
        le16(&mut p, kind);
        le16(&mut p, flags);
        le32(&mut p, body.len() as u32);
        p.extend(body);
        p
    }
    fn local_text() -> String {
        format!("local 🦀\n@{}", "ż".repeat(2000))
    }
    fn remote_text() -> String {
        format!("zdalne żółć 🦀{}", "ę".repeat(2000))
    }
    fn server_clip(s: &mut TcpStream, p: &[u8]) {
        let count = p.len().div_ceil(1600);
        for (i, chunk) in p.chunks(1600).enumerate() {
            let mut payload = vec![];
            le32(&mut payload, p.len() as u32);
            le32(
                &mut payload,
                u32::from(i == 0) | if i + 1 == count { 2 } else { 0 },
            );
            payload.extend(chunk);
            let mut wire = indication(&payload);
            wire[10..12].copy_from_slice(&1006u16.to_be_bytes());
            s.write_all(&wire).unwrap();
        }
    }
    fn read_clip(s: &mut TcpStream) -> Vec<u8> {
        let mut output = vec![];
        loop {
            let p = wire::read_data(s).unwrap();
            assert_eq!(u16::from_be_bytes([p[3], p[4]]), 1006);
            let mut r = Reader::new(&p);
            r.take(6).unwrap();
            let len = r.per_length().unwrap();
            let mut r = Reader::new(r.take(len).unwrap());
            let total = r.u32().unwrap() as usize;
            let flags = r.u32().unwrap();
            if output.is_empty() {
                assert_ne!(flags & 1, 0);
            }
            output.extend(r.rest());
            if flags & 2 != 0 {
                assert_eq!(output.len(), total);
                return output;
            }
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut o = options(&listener, true);
    o.clipboard = true;
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        activate_channels(&mut s, 2, 2, false, true);
        s.write_all(&indication(&data(40, &[0; 8]))).unwrap();
        server_clip(
            &mut s,
            &clip(7, 0, &[1, 0, 0, 0, 1, 0, 12, 0, 2, 0, 0, 0, 2, 0, 0, 0]),
        );
        server_clip(&mut s, &clip(1, 0, &[]));
        assert_eq!(&read_clip(&mut s)[..2], &7u16.to_le_bytes());
        assert_eq!(read_clip(&mut s), clip(2, 0, &[]));
        server_clip(&mut s, &clip(3, 1, &[]));
        let formats = read_clip(&mut s);
        assert_eq!(&formats[8..12], &13u32.to_le_bytes());
        server_clip(&mut s, &clip(3, 1, &[]));
        server_clip(&mut s, &clip(4, 0, &13u32.to_le_bytes()));
        let expected: Vec<u8> = local_text()
            .replace("\n", "\r\n")
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(read_clip(&mut s), clip(5, 1, &expected));
        server_clip(&mut s, &formats);
        assert_eq!(read_clip(&mut s), clip(3, 1, &[]));
        assert_eq!(read_clip(&mut s), clip(4, 0, &13u32.to_le_bytes()));
        let remote: Vec<u8> = remote_text()
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        server_clip(&mut s, &clip(5, 1, &remote));
        let control = client_payload(&mut s);
        assert_eq!(&control[28..32], &[0, 0, 0x1d, 0]);
        let formats = read_clip(&mut s);
        assert_eq!(&formats[..2], &2u16.to_le_bytes());
        // The new local clipboard is served before V reaches the remote application.
        server_clip(&mut s, &clip(4, 0, &13u32.to_le_bytes()));
        let expected: Vec<u8> = "fresh @"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(read_clip(&mut s), clip(5, 1, &expected));
        server_clip(&mut s, &clip(3, 1, &[]));
        let paste = client_payload(&mut s);
        assert_eq!(&paste[18..22], &[2, 0, 0, 0]);
        assert_eq!(&paste[28..32], &[0, 0, 0x2f, 0]);
        assert_eq!(&paste[40..44], &[0, 0x80, 0x2f, 0]);
        let release = client_payload(&mut s);
        assert_eq!(&release[28..32], &[0, 0x80, 0x1d, 0]);
        done_tx.send(()).unwrap();
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        assert!(Instant::now() < deadline, "clipboard peer stalled");
        if done_rx.try_recv().is_ok() {
            break;
        }
        match c.events.recv_timeout(Duration::from_millis(20)) {
            Ok(Event::ClipboardReady) => c.set_clipboard(Some(local_text()), None).unwrap(),
            Ok(Event::ClipboardText(text)) => {
                assert_eq!(text, remote_text());
                c.send(Input::Scancode {
                    code: 0x1d,
                    extended: false,
                    released: false,
                });
                c.send(Input::Paste);
                c.send(Input::Scancode {
                    code: 0x1d,
                    extended: false,
                    released: true,
                });
            }
            Ok(Event::ClipboardSync(request)) => c
                .set_clipboard(Some("fresh @".into()), Some(request))
                .unwrap(),
            Ok(Event::Error(e) | Event::ClipboardWarning(e)) => panic!("{e}"),
            _ => {}
        }
    }
    drop(c);
    peer.join().unwrap();
}
#[test]
fn refuses_plaintext_before_username() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let o = options(&listener, false);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        setup(&mut s, false);
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    let mut rejected = false;
    while let Ok(e) = c.events.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::Error(e) => {
                assert!(e.contains("plaintext"));
                rejected = true;
            }
            Event::Closed => break,
            _ => {}
        }
    }
    assert!(rejected);
    peer.join().unwrap();
}
#[test]
fn refuses_unpinned_legacy_even_with_plaintext_opt_in() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let o = options(&listener, true);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        setup(&mut s, true);
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    let mut rejected = false;
    while let Ok(e) = c.events.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::Error(e) => {
                assert!(e.contains("encryption"));
                rejected = true;
            }
            Event::Closed => break,
            _ => {}
        }
    }
    assert!(rejected);
    peer.join().unwrap();
}
fn activate(s: &mut TcpStream, width: u16, height: u16) {
    activate_graphics(s, width, height, false);
}
fn activate_graphics(s: &mut TcpStream, width: u16, height: u16, graphics: bool) -> bool {
    activate_channels(s, width, height, graphics, false)
}
fn activate_channels(
    s: &mut TcpStream,
    width: u16,
    height: u16,
    graphics: bool,
    clipboard: bool,
) -> bool {
    let requested_graphics = setup_channels(s, false, graphics, clipboard);
    assert_eq!(wire::read_data(s).unwrap(), [4, 1, 0, 1, 0]);
    assert_eq!(wire::read_data(s).unwrap(), [0x28]);
    wire::write_data(s, &[0x2e, 0, 0, 4]).unwrap();
    let mut channels = vec![1005u16, 1003];
    if graphics {
        channels.push(1006);
    }
    if clipboard {
        channels.push(1006 + u16::from(graphics));
    }
    if graphics {
        channels.push(1007 + u16::from(clipboard));
    }
    for ch in channels {
        let p = wire::read_data(s).unwrap();
        assert_eq!(p[0], 0x38);
        assert_eq!(u16::from_be_bytes([p[3], p[4]]), ch);
        let mut response = vec![0x3e, 0, 0, 4];
        response.extend(ch.to_be_bytes());
        response.extend(ch.to_be_bytes());
        wire::write_data(s, &response).unwrap();
    }
    let info = client_payload(s);
    assert_eq!(&info[..4], &[0x40, 0, 0, 0]);
    assert_ne!(info[8] & 0x10, 0);
    let flags = u32::from_le_bytes(info[8..12].try_into().unwrap());
    assert_eq!(flags & 0x1e80, 0x0280); // MPPC, maximum 64K.
    assert!(info.windows(12).any(|s| s == b"t\0e\0s\0t\0e\0r\0"));
    if graphics {
        // The server withholds licensing/desktop until its mandatory network
        // detection exchange completes on the separately joined message channel.
        s.write_all(&message_indication(&[6, 0, 42, 0, 1, 16]))
            .unwrap();
        assert_eq!(message_reply(s), [0, 32, 0, 0, 6, 1, 42, 0, 0, 0]);
        let mut probes = message_indication(&[6, 0, 43, 0, 20, 16]);
        probes.extend(message_indication(&[8, 0, 44, 0, 2, 0, 4, 0, 1, 2, 3, 4]));
        probes.extend(message_indication(&[8, 0, 45, 0, 43, 0, 4, 0, 5, 6, 7, 8]));
        s.write_all(&probes).unwrap();
        let result = message_reply(s);
        assert_eq!(&result[..10], &[0, 32, 0, 0, 14, 1, 45, 0, 3, 0]);
        assert_eq!(&result[14..18], &24u32.to_le_bytes());
    }
    s.write_all(&indication(&[
        0x80, 2, 0, 0, 0xff, 3, 16, 0, 7, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0,
    ]))
    .unwrap();
    // Minimal Demand Active: one Bitmap capability.
    let mut cap = vec![];
    le16(&mut cap, 2);
    le16(&mut cap, 28);
    for n in [24, 1, 1, 1, width, height, 0, 1, 0, 0, 1, 0] {
        le16(&mut cap, n);
    }
    let mut demand = vec![];
    le32(&mut demand, 0x10001);
    le16(&mut demand, 4);
    le16(&mut demand, (cap.len() + 4) as u16);
    demand.extend(b"srv\0");
    le16(&mut demand, 1);
    le16(&mut demand, 0);
    demand.extend(cap);
    le32(&mut demand, 0);
    let p = indication(&share(1, &demand));
    // Fragment mid-TPKT and mid-payload across separate TCP writes.
    for chunk in p.chunks(3) {
        s.write_all(chunk).unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let confirm = client_payload(s);
    assert_eq!(confirm[2] & 15, 3);
    for kind in [31, 20, 20, 39] {
        let p = client_payload(s);
        assert_eq!(p[14], kind);
        assert_eq!(u16::from_le_bytes([p[12], p[13]]) as usize, p.len());
    }
    requested_graphics
}
#[test]
fn fragmented_activation_bitmap_and_input_roundtrip() {
    activation_bitmap_and_input(None);
}
#[test]
fn compressed_activation_bitmap_and_input_roundtrip() {
    for level in [0, 1] {
        activation_bitmap_and_input(Some(level));
    }
}
fn activation_bitmap_and_input(compression: Option<u8>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let o = options(&listener, true);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        activate(&mut s, 2, 2);
        let mut bitmap = vec![1, 0, 1, 0];
        for n in [0, 0, 0, 0, 1, 1, 24, 0, 4] {
            le16(&mut bitmap, n);
        }
        bitmap.extend([1, 2, 3, 0]);
        let mut output = indication(&data(40, &[0; 8]));
        // Pointer updates are Share Data PDUs, independent of desktop pixels.
        let mut pointer: Vec<u8> = [6u16, 0, 3, 0, 0, 1, 1, 2, 4]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        pointer.extend([0, 0, 0, 0, 0, 0]);
        output.extend(indication(&data(27, &pointer)));
        output.extend(indication(&data(27, &[7, 0, 0, 0, 3, 0])));
        output.extend(indication(&data(27, &[1, 0, 0, 0, 0, 0, 0, 0])));
        output.extend(indication(&data(27, &[1, 0, 0, 0, 0, 0x7f, 0, 0])));

        // Frame START precedes a bitmap in another PDU; the graphics worker
        // must keep it off the UI until the matching END after the screen copy.
        output.extend(indication(&data(
            2,
            &[0, 0, 0, 0, 1, 0, 0, 0, 0x36, 0, 0, 0, 0],
        )));
        if let Some(level) = compression {
            // All-literal first packet seeds the dictionary independently of
            // our decoder. The next packet is one overlapping history token.
            let mut bits = String::new();
            for byte in &bitmap {
                if *byte < 128 {
                    bits.push_str(&format!("{byte:08b}"));
                } else {
                    bits.push_str(&format!("10{:07b}", byte & 127));
                }
            }
            let pack = |bits: String| {
                let mut bytes = vec![0; bits.len().div_ceil(8)];
                for (i, bit) in bits.bytes().enumerate() {
                    bytes[i / 8] |= (bit - b'0') << (7 - i % 8);
                }
                bytes
            };
            let compressed = |flags, bytes: Vec<u8>| {
                let mut p = data(2, &bytes);
                p[12..14].copy_from_slice(&((bitmap.len() + 18) as u16).to_le_bytes());
                p[15] = flags;
                p[16..18].copy_from_slice(&((bytes.len() + 18) as u16).to_le_bytes());
                p
            };
            output.extend(indication(&compressed(0xe0 | level, pack(bits))));
            assert_eq!(bitmap.len(), 26);
            let prefix = if level == 0 { "1111" } else { "11111" };
            // Offset 26, length 26: 1110 + 4-bit (26-16).
            let repeat = pack(format!("{prefix}01101011101010"));
            output.extend(indication(&compressed(0x20 | level, repeat)));
        } else {
            output.extend(indication(&data(2, &bitmap)));
        }
        // Scroll/copy the first pixel into the adjacent pixel using a primary
        // order after the bitmap. The async worker must preserve this ordering.
        let mut orders = vec![0, 0, 0, 0, 2, 0, 0, 0, 0x09, 2, 0x7f];
        for n in [1u16, 0, 1, 1] {
            le16(&mut orders, n);
        }
        orders.push(0xcc);
        for n in [0u16, 0] {
            le16(&mut orders, n);
        }
        orders.extend([0x36, 1, 0, 0, 0]); // Frame END, second order.
        output.extend(indication(&data(2, &orders)));
        s.write_all(&output).unwrap();
        let p = client_payload(&mut s);
        assert_eq!(p[14], 28);
        assert_eq!(&p[28..32], &[0, 0, 0x1e, 0]);
        let p = client_payload(&mut s);
        assert_eq!(&p[28..32], &[0, 0x80, 0x1e, 0]);
        // Keep the peer alive until the test has inspected its shared frame.
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    let mut active = false;
    while let Ok(e) = c.events.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::Active => {
                active = true;
                break;
            }
            Event::Error(e) => panic!("{e}"),
            Event::Closed => panic!("closed before activation"),
            _ => {}
        }
    }
    assert!(active);
    for expected in 0..4 {
        let event = c.events.recv_timeout(Duration::from_secs(5)).unwrap();
        match (expected, event) {
            (0 | 1, Event::Pointer(rumstsc_rdp::pointer::Pointer::Image(image))) => {
                assert_eq!((image.width, image.height), (1, 1));
                assert_eq!(image.bgra, [0, 0, 0, 255]);
            }
            (2, Event::Pointer(rumstsc_rdp::pointer::Pointer::Hidden)) => {}
            (3, Event::Pointer(rumstsc_rdp::pointer::Pointer::Default)) => {}
            (_, event) => panic!("unexpected pointer event: {event:?}"),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while c.frame.lock().unwrap().revision < if compression.is_some() { 3 } else { 2 } {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        &c.frame.lock().unwrap().bgra[..8],
        &[1, 2, 3, 255, 1, 2, 3, 255]
    );
    assert_eq!(c.frame.lock().unwrap().screen_blits, 1);
    assert_eq!(c.frame.lock().unwrap().logical_frames, 1);
    c.request_snapshot(0, 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(snapshot) = c.take_snapshot() {
            assert_eq!(&snapshot.tiles[0].bgra[..8], &[1, 2, 3, 255, 1, 2, 3, 255]);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    for released in [false, true] {
        c.send(Input::Scancode {
            code: 0x1e,
            extended: false,
            released,
        });
    }
    thread::sleep(Duration::from_millis(80));
    c.disconnect();
    peer.join().unwrap();
}
#[test]
fn cancellation_interrupts_a_blocked_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let o = options(&listener, false);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        // Disconnect may win the race before the client finishes the first
        // TPKT (especially on Windows CI). Either a full header then EOF, or
        // UnexpectedEof mid-read, means the handshake was cancelled.
        match wire::read_tpkt(&mut s) {
            Ok(_) => {
                let mut b = [0];
                assert_eq!(s.read(&mut b).unwrap(), 0);
            }
            Err(rumstsc_rdp::Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
            Err(e) => panic!("expected TPKT or cancel EOF, got {e}"),
        }
    });
    let c = Connection::start(o).unwrap();
    thread::sleep(Duration::from_millis(80));
    c.disconnect();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < deadline);
        if matches!(
            c.events.recv_timeout(Duration::from_millis(100)),
            Ok(Event::Closed)
        ) {
            break;
        }
    }
    peer.join().unwrap();
}

/// Local transport/decoder benchmark, independent of server bandwidth and UI GPU.
#[test]
#[ignore = "manual throughput measurement; wall-clock performance depends on host load"]
fn bulk_bitmap_receive_throughput() {
    const UPDATES: u64 = 320;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut o = options(&listener, true);
    o.width = 1920;
    o.height = 1080;
    let mut bitmap = vec![1, 0, 1, 0];
    for n in [0, 0, 1919, 3, 1920, 4, 24, 0, 1920 * 4 * 3] {
        le16(&mut bitmap, n);
    }
    bitmap.extend([1, 2, 3].repeat(1920 * 4));
    let packet = indication(&data(2, &bitmap));
    let total_bytes = packet.len() * UPDATES as usize;
    let burst = packet.repeat(UPDATES as usize);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        s.set_nodelay(true).unwrap();
        activate(&mut s, 1920, 1080);
        s.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        s.write_all(&indication(&data(40, &[0; 8]))).unwrap();
        s.write_all(&burst).unwrap();
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    loop {
        match c.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Active => break,
            Event::Error(e) => panic!("{e}"),
            Event::Closed => panic!("closed before activation"),
            _ => {}
        }
    }
    let start = Instant::now();
    loop {
        let frame = c.frame.lock().unwrap();
        if frame.revision == UPDATES {
            assert_eq!(&frame.bgra[..4], &[1, 2, 3, 255]);
            break;
        }
        drop(frame);
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "bitmap burst stalled"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let elapsed = start.elapsed();
    println!(
        "{UPDATES} bitmap updates, {total_bytes} bytes in {:.3}s ({:.1} MiB/s)",
        elapsed.as_secs_f64(),
        total_bytes as f64 / 1048576.0 / elapsed.as_secs_f64()
    );
    c.disconnect();
    peer.join().unwrap();
}

fn gfx_packet(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut p = vec![];
    le16(&mut p, kind);
    le16(&mut p, 0);
    le32(&mut p, (body.len() + 8) as u32);
    p.extend(body);
    p
}
fn message_indication(request: &[u8]) -> Vec<u8> {
    let mut payload = vec![0, 16, 0, 0];
    payload.extend(request);
    let mut p = vec![2, 0xf0, 0x80, 0x68, 0, 1, 3, 0xef, 0x70];
    wire::per_length(&mut p, payload.len()).unwrap();
    p.extend(payload);
    wire::tpkt(&p).unwrap()
}
fn message_reply(s: &mut TcpStream) -> Vec<u8> {
    let p = wire::read_data(s).unwrap();
    let mut r = Reader::new(&p);
    assert_eq!(r.u8().unwrap(), 0x64);
    r.be16().unwrap();
    assert_eq!(
        r.be16().unwrap(),
        1007,
        "auto-detect response must use the message channel"
    );
    assert_eq!(r.u8().unwrap(), 0x70);
    let n = r.per_length().unwrap();
    r.take(n).unwrap().to_vec()
}
fn static_indication(packet: &[u8]) -> Vec<u8> {
    let mut payload = vec![];
    le32(&mut payload, packet.len() as u32);
    le32(&mut payload, 3);
    payload.extend(packet);
    let mut p = vec![2, 0xf0, 0x80, 0x68, 0, 1, 3, 0xee, 0x70];
    wire::per_length(&mut p, payload.len()).unwrap();
    p.extend(payload);
    wire::tpkt(&p).unwrap()
}
fn graphics_reply(s: &mut TcpStream) -> Vec<u8> {
    let p = wire::read_data(s).unwrap();
    let mut r = Reader::new(&p);
    assert_eq!(r.u8().unwrap(), 0x64);
    r.be16().unwrap();
    assert_eq!(r.be16().unwrap(), 1006);
    assert_eq!(r.u8().unwrap(), 0x70);
    let n = r.per_length().unwrap();
    let mut r = Reader::new(r.take(n).unwrap());
    let size = r.u32().unwrap() as usize;
    assert_eq!(r.u32().unwrap(), 3);
    assert_eq!(r.remaining(), size);
    r.rest().to_vec()
}
fn send_gfx(s: &mut TcpStream, bytes: &[u8]) {
    let mut compressed = vec![0xe0, 4];
    compressed.extend(bytes);
    for (i, chunk) in compressed.chunks(700).enumerate() {
        let mut p = vec![
            if i == 0 && compressed.len() > 700 {
                0x28
            } else {
                0x30
            },
            1,
        ];
        if i == 0 && compressed.len() > 700 {
            le32(&mut p, compressed.len() as u32);
        }
        p.extend(chunk);
        s.write_all(&static_indication(&p)).unwrap();
    }
}
#[test]
fn dynamic_graphics_avc_frames_cache_and_acknowledgements() {
    if !rumstsc_video::available() {
        eprintln!("Native video unavailable; skip AVC integration");
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut o = options(&listener, true);
    o.width = 64;
    o.height = 64;
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let peer = thread::spawn(move || {
        let mut s = accept(listener);
        s.set_nodelay(true).unwrap();
        activate_graphics(&mut s, 64, 64, true);
        s.write_all(&indication(&data(40, &[0; 8]))).unwrap();
        // Match the real stalled peer: do not open graphics until v2 is selected.
        s.write_all(&static_indication(&[0x50, 0, 2, 0, 1, 0, 2, 0, 3, 0, 4, 0]))
            .unwrap();
        assert_eq!(graphics_reply(&mut s), [0x50, 0, 2, 0]);
        let mut create = vec![0x10, 1];
        create.extend(b"Microsoft::Windows::RDS::Graphics\0");
        s.write_all(&static_indication(&create)).unwrap();
        let created = graphics_reply(&mut s);
        assert_eq!(created[0], 0x12);
        assert_eq!(&created[5..], &[0, 0, 0, 0]);
        let caps = graphics_reply(&mut s);
        assert_eq!(&caps[5..7], &[0x12, 0]);
        assert_eq!(&caps[15..19], &0x80105u32.to_le_bytes());
        let mut output = gfx_packet(0x13, &[5, 1, 8, 0, 4, 0, 0, 0, 0x12, 0, 0, 0]);
        let mut reset = vec![0; 332];
        reset[..4].copy_from_slice(&64u32.to_le_bytes());
        reset[4..8].copy_from_slice(&64u32.to_le_bytes());
        output.extend(gfx_packet(14, &reset));
        output.extend(gfx_packet(9, &[1, 0, 64, 0, 64, 0, 0x20]));
        output.extend(gfx_packet(15, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        output.extend(gfx_packet(11, &[0, 0, 0, 0, 100, 0, 0, 0]));
        let video = |encoded: &[u8], rect: [u16; 4]| {
            let mut metadata = vec![];
            le32(&mut metadata, 1);
            for v in rect {
                le16(&mut metadata, v);
            }
            metadata.extend([0, 100]);
            metadata.extend(encoded);
            let mut body = vec![1, 0, 11, 0, 0x20];
            for v in [0, 0, 64, 64] {
                le16(&mut body, v);
            }
            le32(&mut body, metadata.len() as u32);
            body.extend(metadata);
            gfx_packet(1, &body)
        };
        output.extend(video(
            include_bytes!("../../video/tests/data/gray-idr.h264"),
            [0, 0, 64, 64],
        ));
        output.extend(gfx_packet(12, &100u32.to_le_bytes()));
        send_gfx(&mut s, &output);
        let ack = graphics_reply(&mut s);
        assert_eq!(&ack[5..7], &13u16.to_le_bytes());
        assert_eq!(&ack[17..21], &100u32.to_le_bytes());
        let mut output = gfx_packet(11, &[0, 0, 0, 0, 101, 0, 0, 0]);
        output.extend(gfx_packet(
            4,
            &[1, 0, 7, 8, 9, 255, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0],
        ));
        output.extend(video(
            include_bytes!("../../video/tests/data/gray-p.h264"),
            [16, 16, 48, 48],
        ));
        output.extend(gfx_packet(
            6,
            &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0],
        ));
        output.extend(gfx_packet(7, &[1, 0, 1, 0, 1, 0, 1, 0, 0, 0]));
        output.extend(gfx_packet(
            5,
            &[1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 1, 0, 2, 0, 0, 0],
        ));
        output.extend(gfx_packet(12, &101u32.to_le_bytes()));
        send_gfx(&mut s, &output);
        let ack = graphics_reply(&mut s);
        assert_eq!(&ack[17..21], &101u32.to_le_bytes());
        assert_eq!(&ack[21..25], &2u32.to_le_bytes());
        ready_tx.send(()).unwrap();
        let mut b = [0];
        assert_eq!(s.read(&mut b).unwrap(), 0);
    });
    let c = Connection::start(o).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        while let Ok(event) = c.events.try_recv() {
            if let Event::Error(error) = event {
                panic!("{error}");
            }
        }
        if c.frame.lock().unwrap().logical_frames == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "GFX frame timeout");
        thread::sleep(Duration::from_millis(2));
    }
    let frame = c.frame.lock().unwrap();
    assert_eq!(&frame.bgra[..12], &[7, 8, 9, 255].repeat(3));
    assert!(
        frame.bgra[12..]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0].abs_diff(100) <= 2 && p[3] == 255)
    );
    drop(frame);
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    c.disconnect();
    peer.join().unwrap();
}
