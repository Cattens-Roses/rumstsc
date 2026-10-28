//! Headless negotiation probe and explicit opt-in session/bitmap smoke test.
use rumstsc_rdp::{
    mcs,
    negotiation::{Endpoint, Security, SecurityMode},
    session::{Connection, Event, Options},
    transport::Transport,
    wire,
};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> rumstsc_rdp::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut host = None;
    let mut mode = SecurityMode::Auto;
    let mut session = false;
    let mut output = None;
    let mut trace_path = None;
    let mut pin = None;
    let mut user = String::new();
    let mut domain = String::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "Usage: rumstsc-probe [--mode auto|tls|nla|legacy] [--pin SHA256] [--session] [--output FRAME.ppm] [--trace TRACE.json] [--user USER] [--domain DOMAIN] HOST[:PORT]\nOnly --session starts a desktop session. For authentication, read RUMSTSC_PASSWORD from the environment; passwords are never accepted on the command line."
                );
                return Ok(());
            }
            "--session" => session = true,
            "--output" => output = args.next(),
            "--trace" => {
                trace_path = Some(std::path::PathBuf::from(args.next().ok_or_else(|| {
                    rumstsc_rdp::Error::Protocol("missing --trace path".into())
                })?))
            }
            "--user" => user = args.next().unwrap_or_default(),
            "--domain" => domain = args.next().unwrap_or_default(),
            "--pin" => pin = rumstsc_rdp::crypto::parse_pin(&args.next().unwrap_or_default())?,
            "--mode" => {
                mode = match args.next().as_deref() {
                    Some("auto") => SecurityMode::Auto,
                    Some("tls") => SecurityMode::Tls,
                    Some("nla") => SecurityMode::Nla,
                    Some("legacy") => SecurityMode::Legacy,
                    _ => return Err(rumstsc_rdp::Error::Protocol("invalid --mode".into())),
                }
            }
            _ if !arg.starts_with('-') && host.is_none() => host = Some(arg),
            _ => {
                return Err(rumstsc_rdp::Error::Protocol(
                    "unknown command-line option; use --help".into(),
                ));
            }
        }
    }
    let endpoint = Endpoint::parse(
        &host.ok_or_else(|| rumstsc_rdp::Error::Protocol("missing host; use --help".into()))?,
    )?;
    if session {
        let connection = Connection::start_traced(
            Options {
                endpoint,
                username: user,
                domain,
                password: std::env::var("RUMSTSC_PASSWORD").unwrap_or_default().into(),
                security_mode: mode,
                certificate_pin: pin,
                width: 1280,
                height: 720,
                allow_insecure: false,
                clipboard: false,
            },
            trace_path.clone(),
        )?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
        let mut active = false;
        let mut first_frame = None;
        let mut snapshot_cursor = (0, 0);
        let mut snapshot_pixels = Vec::new();
        let mut snapshot_size = (0u16, 0u16);
        while std::time::Instant::now() < deadline {
            connection.request_snapshot(snapshot_cursor.0, snapshot_cursor.1);
            if let Some(snapshot) = connection.take_snapshot() {
                if snapshot_cursor.0 != snapshot.generation {
                    snapshot_pixels =
                        vec![0; snapshot.width as usize * snapshot.height as usize * 4];
                }
                snapshot_size = (snapshot.width, snapshot.height);
                for tile in snapshot.tiles {
                    for y in 0..tile.height as usize {
                        let dst = ((tile.top as usize + y) * snapshot.width as usize
                            + tile.left as usize)
                            * 4;
                        let src = y * tile.width as usize * 4;
                        let bytes = tile.width as usize * 4;
                        snapshot_pixels[dst..dst + bytes]
                            .copy_from_slice(&tile.bgra[src..src + bytes]);
                    }
                }
                snapshot_cursor = (snapshot.generation, snapshot.revision);
            }
            match connection
                .events
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(Event::Status(s)) => println!("{s}"),
                Ok(Event::Security(s)) => println!("Security: {s}"),
                Ok(Event::Graphics(s)) => println!("Graphics: {s}"),
                Ok(Event::Active) => {
                    active = true;
                    println!("Desktop activated");
                }
                Ok(Event::Error(e)) => return Err(rumstsc_rdp::Error::Protocol(e)),
                Ok(Event::Closed) => {
                    return Err(rumstsc_rdp::Error::Protocol("server closed session".into()));
                }
                _ => {}
            }
            let frame = connection.frame.lock().unwrap();
            if active && !snapshot_pixels.is_empty() {
                let first = first_frame.get_or_insert_with(std::time::Instant::now);
                if first.elapsed() < std::time::Duration::from_millis(500) {
                    continue;
                }
                println!(
                    "Received {} × {} desktop, revision {}",
                    snapshot_size.0, snapshot_size.1, snapshot_cursor.1
                );
                println!("Screen copies: {}", frame.screen_blits);
                println!(
                    "Rectangles: {} compressed, {} raw",
                    frame.compressed_rectangles, frame.raw_rectangles
                );
                let alpha = snapshot_pixels.as_chunks::<4>().0.iter().map(|p| p[3]);
                println!(
                    "Alpha: min {}, max {}",
                    alpha.clone().min().unwrap_or(0),
                    alpha.max().unwrap_or(0)
                );
                if let Some(path) = output {
                    use std::io::Write;
                    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
                    write!(file, "P6\n{} {}\n255\n", snapshot_size.0, snapshot_size.1)?;
                    for p in snapshot_pixels.as_chunks::<4>().0.iter() {
                        file.write_all(&[p[2], p[1], p[0]])?;
                    }
                    file.flush()?;
                }
                drop(frame);
                connection.disconnect();
                while !matches!(
                    connection
                        .events
                        .recv_timeout(std::time::Duration::from_secs(3)),
                    Ok(Event::Closed) | Err(_)
                ) {}
                if let Some(path) = &trace_path {
                    connection.trace.save(path)?;
                    println!("Trace: {}", path.display());
                }
                return Ok(());
            }
        }
        return Err(rumstsc_rdp::Error::Protocol(
            "no desktop bitmap received within 25 seconds".into(),
        ));
    }
    let mut socket = endpoint.connect()?;
    let security = rumstsc_rdp::negotiation::negotiate_mode(&mut socket, mode)?;
    println!("Selected security: {security}");
    let mut stream = if security == Security::Legacy {
        Transport::Plain(socket)
    } else {
        Transport::tls(socket, &endpoint.host, pin)?
    };
    if security != Security::Legacy {
        println!(
            "TLS verified; certificate SHA-256 {}",
            rumstsc_rdp::crypto::fingerprint(stream.certificate()?)
        );
    }
    if security == Security::Nla {
        println!("NLA selected; use --session with a disposable test account to authenticate.");
        return Ok(());
    }
    {
        wire::write_data(
            &mut stream,
            &mcs::connect_initial_with(
                1280,
                720,
                if security == Security::Tls { 1 } else { 0 },
                security == Security::Legacy,
            )?,
        )?;
        let settings = mcs::connect_response(&wire::read_data(&mut stream)?)?;
        println!(
            "Global channel: {}\nEncryption method: 0x{:x}\nEncryption level: {}",
            settings.channel, settings.encryption_method, settings.encryption_level
        );
        if settings.encryption_method != 0 || settings.encryption_level != 0 {
            println!(
                "Legacy certificate SHA-256 {}",
                rumstsc_rdp::crypto::fingerprint(&settings.certificate)
            );
        }
    }
    Ok(())
}
