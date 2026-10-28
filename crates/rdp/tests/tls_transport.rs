use rumstsc_rdp::{crypto, transport::Transport};
use rustls::{ServerConfig, ServerConnection, StreamOwned, pki_types::PrivatePkcs8KeyDer};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::Duration,
};
fn pair() -> (TcpListener, Vec<u8>, Arc<ServerConfig>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = cert.cert.der().to_vec();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
            )
            .unwrap();
    (
        TcpListener::bind("127.0.0.1:0").unwrap(),
        der,
        Arc::new(config),
    )
}
#[test]
fn pinned_tls_application_bytes() {
    let (listener, cert, config) = pair();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        let mut data = [0; 4];
        tls.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"TPKT");
        tls.write_all(b"reply").unwrap();
        tls.flush().unwrap();
    });
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut tls = Transport::tls(socket, "localhost", Some(crypto::sha256(&[&cert]))).unwrap();
    assert_eq!(tls.certificate().unwrap(), cert);
    assert!(!tls.public_key().unwrap().is_empty());
    tls.write_all(b"TPKT").unwrap();
    let mut reply = [0; 5];
    tls.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"reply");
    peer.join().unwrap();
}
#[test]
fn mismatched_pin_never_sends_application_data() {
    let (listener, _, config) = pair();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        let mut data = [0; 1];
        assert!(tls.read(&mut data).is_err());
    });
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    assert!(Transport::tls(socket, "localhost", Some([0; 32])).is_err());
    peer.join().unwrap();
}
#[test]
fn private_certificate_is_not_implicitly_trusted() {
    let (listener, _, config) = pair();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        let mut data = [0; 1];
        assert!(tls.read(&mut data).is_err());
    });
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(Transport::tls(socket, "localhost", None).is_err());
    peer.join().unwrap();
}

#[test]
fn certificate_review_blocks_until_explicit_approval() {
    use std::sync::mpsc;
    let (listener, cert, config) = pair();
    let address = listener.local_addr().unwrap();
    let (received, reviews) = mpsc::channel();
    let (approve, decisions) = mpsc::channel();
    let decisions = std::sync::Mutex::new(decisions);
    let (data_tx, data_rx) = mpsc::channel();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        let mut data = [0; 4];
        tls.read_exact(&mut data).unwrap();
        data_tx.send(data).unwrap();
        tls.write_all(b"ok").unwrap();
        tls.flush().unwrap();
    });
    let client = thread::spawn(move || {
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut tls = Transport::tls_with_review(
            socket,
            "localhost",
            None,
            Some(Arc::new(move |review| {
                received.send(review).unwrap();
                decisions
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
            })),
        )
        .unwrap();
        tls.write_all(b"TPKT").unwrap();
        let mut reply = [0; 2];
        tls.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"ok");
    });
    let review = reviews.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(review.host, "localhost");
    assert_eq!(review.fingerprint, crypto::fingerprint(&cert));
    assert!(!review.subject.is_empty());
    assert_eq!(review.subject, review.issuer);
    assert!(review.reason.contains("UnknownIssuer"));
    assert!(data_rx.recv_timeout(Duration::from_millis(200)).is_err());
    approve.send(true).unwrap();
    assert_eq!(
        data_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        *b"TPKT"
    );
    client.join().unwrap();
    peer.join().unwrap();
}

#[test]
fn declining_certificate_review_sends_no_application_bytes() {
    let (listener, _, config) = pair();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        assert!(tls.read(&mut [0; 1]).is_err());
    });
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(
        Transport::tls_with_review(socket, "localhost", None, Some(Arc::new(|_| false))).is_err()
    );
    peer.join().unwrap();
}

#[test]
fn explicit_pin_mismatch_cannot_be_overridden_by_review() {
    let (listener, _, config) = pair();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        assert!(tls.read(&mut [0; 1]).is_err());
    });
    let socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(
        Transport::tls_with_review(
            socket,
            "localhost",
            Some([0; 32]),
            Some(Arc::new(|_| {
                panic!("a mismatched explicit pin must never prompt for trust")
            }))
        )
        .is_err()
    );
    peer.join().unwrap();
}

#[test]
fn cancelling_interactive_review_stops_worker() {
    use rumstsc_rdp::{
        negotiation::{Endpoint, SecurityMode},
        session::{Connection, Event, Options},
        wire,
    };
    let (listener, _, config) = pair();
    let options = Options {
        endpoint: Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap(),
        username: "test".into(),
        domain: String::new(),
        password: "never-delegated".to_string().into(),
        security_mode: SecurityMode::Nla,
        certificate_pin: None,
        width: 2,
        height: 2,
        allow_insecure: false,
        clipboard: false,
    };
    let peer = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        wire::read_tpkt(&mut s).unwrap();
        // X.224 confirm selecting NLA.
        s.write_all(&[3, 0, 0, 19, 14, 0xd0, 0, 0, 0, 0, 0, 2, 0, 8, 0, 2, 0, 0, 0])
            .unwrap();
        let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), s);
        assert!(tls.read(&mut [0; 1]).is_err());
    });
    let connection = Connection::start_interactive(options).unwrap();
    loop {
        match connection
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
        {
            Event::CertificateReview(_) => break,
            Event::Error(e) => panic!("{e}"),
            Event::Status(_) => {}
            event => panic!("unexpected event: {event:?}"),
        }
    }
    connection.disconnect();
    assert!(matches!(
        connection
            .events
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        Event::Closed
    ));
    peer.join().unwrap();
}
