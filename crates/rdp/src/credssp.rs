//! CredSSP v5/v6 over an already authenticated TLS channel (MS-CSSP).
//! NTLMv2 is encoded here; TLS and hash/cipher primitives are separate.
use crate::{
    Error, Result, crypto, invalid, ntlm,
    transport::Transport,
    wire::{self, Reader},
};
use std::io::{Read, Write};
use zeroize::Zeroizing;
fn sequence(data: &[u8]) -> Vec<u8> {
    wire::ber(&[0x30], data)
}
fn octet(data: &[u8]) -> Vec<u8> {
    wire::ber(&[4], data)
}
fn context(tag: u8, data: &[u8]) -> Vec<u8> {
    wire::ber(&[0xa0 + tag], data)
}
fn integer(value: u32) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(3);
    let mut n = bytes[start..].to_vec();
    if n[0] & 128 != 0 {
        n.insert(0, 0);
    }
    wire::ber(&[2], &n)
}
pub fn request(
    version: u32,
    token: Option<&[u8]>,
    auth: Option<&[u8]>,
    key: Option<&[u8]>,
    nonce: Option<&[u8]>,
) -> Vec<u8> {
    let mut fields = context(0, &integer(version));
    if let Some(token) = token {
        fields.extend(context(1, &sequence(&sequence(&context(0, &octet(token))))));
    }
    if let Some(auth) = auth {
        fields.extend(context(2, &octet(auth)));
    }
    if let Some(key) = key {
        fields.extend(context(3, &octet(key)));
    }
    if let Some(nonce) = nonce {
        fields.extend(context(5, &octet(nonce)));
    }
    sequence(&fields)
}
#[derive(Debug)]
pub struct Response {
    pub version: u32,
    pub token: Option<Vec<u8>>,
    pub key: Option<Vec<u8>>,
}
fn one<'a>(bytes: &'a [u8], tag: &[u8]) -> Result<&'a [u8]> {
    let mut r = Reader::new(bytes);
    let value = r.ber(tag)?;
    if r.remaining() != 0 {
        return Err(invalid("trailing ASN.1 field"));
    }
    Ok(value)
}
fn number(bytes: &[u8]) -> Result<u32> {
    let b = one(bytes, &[2])?;
    if b.is_empty() || b.len() > 5 || (b.len() == 5 && b[0] != 0) {
        return Err(invalid("ASN.1 integer length"));
    }
    Ok(b.iter().fold(0u32, |n, b| (n << 8) | *b as u32))
}
fn tlv<'a>(r: &mut Reader<'a>) -> Result<(u8, &'a [u8])> {
    let tag = r.u8()?;
    let first = r.u8()?;
    let n = if first & 128 == 0 {
        first as usize
    } else {
        let count = first & 127;
        if count == 0 || count > 3 {
            return Err(invalid("ASN.1 length"));
        }
        let mut n = 0;
        for b in r.take(count as usize)? {
            n = (n << 8) | *b as usize;
        }
        n
    };
    Ok((tag, r.take(n)?))
}
pub fn response(packet: &[u8]) -> Result<Response> {
    let mut r = Reader::new(one(packet, &[0x30])?);
    let mut version = None;
    let mut token = None;
    let mut key = None;
    let mut fields = std::collections::BTreeSet::new();
    while r.remaining() > 0 {
        let (tag, body) = tlv(&mut r)?;
        if !fields.insert(tag) {
            return Err(invalid("duplicate CredSSP field"));
        }
        match tag {
            0xa0 => version = Some(number(body)?),
            0xa1 => {
                token = Some(one(one(one(one(body, &[0x30])?, &[0x30])?, &[0xa0])?, &[4])?.to_vec())
            }
            0xa3 => key = Some(one(body, &[4])?.to_vec()),
            0xa4 => {
                let status = number(body)?;
                if status != 0 {
                    return Err(Error::Security(format!(
                        "CredSSP authentication rejected (NTSTATUS 0x{status:08x})"
                    )));
                }
            }
            0xa5 => {
                if one(body, &[4])?.len() != 32 {
                    return Err(invalid("CredSSP nonce length"));
                }
            }
            _ => return Err(invalid("unexpected CredSSP response field")),
        }
    }
    let version = version.ok_or_else(|| invalid("missing CredSSP version"))?;
    if version < 5 {
        return Err(Error::Security(
            "CredSSP versions before v5 are refused (missing nonce-bound server proof)".into(),
        ));
    }
    Ok(Response {
        version: version.min(6),
        token,
        key,
    })
}
pub fn read_message(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut h = [0; 2];
    stream.read_exact(&mut h)?;
    if h[0] != 0x30 {
        return Err(invalid("expected CredSSP ASN.1 sequence"));
    }
    let mut p = h.to_vec();
    let n = if h[1] & 128 == 0 {
        h[1] as usize
    } else {
        let count = (h[1] & 127) as usize;
        if count == 0 || count > 3 {
            return Err(invalid("CredSSP frame length"));
        }
        let mut b = vec![0; count];
        stream.read_exact(&mut b)?;
        let n = b.iter().fold(0usize, |n, b| (n << 8) | *b as usize);
        p.extend(b);
        n
    };
    if n > 65535 {
        return Err(invalid("CredSSP frame exceeds limit"));
    }
    let start = p.len();
    p.resize(start + n, 0);
    stream.read_exact(&mut p[start..])?;
    Ok(p)
}
fn initial_spnego(token: &[u8]) -> Vec<u8> {
    let ntlm_oid = wire::ber(&[6], &[0x2b, 6, 1, 4, 1, 0x82, 0x37, 2, 2, 0x0a]);
    let mut init = context(0, &sequence(&ntlm_oid));
    init.extend(context(2, &octet(token)));
    let mut gss = wire::ber(&[6], &[0x2b, 6, 1, 5, 5, 2]);
    gss.extend(context(0, &sequence(&init)));
    wire::ber(&[0x60], &gss)
}
fn spnego_response(token: &[u8]) -> Vec<u8> {
    context(1, &sequence(&context(2, &octet(token))))
}
fn challenge_token(token: &[u8]) -> Result<Vec<u8>> {
    if token.starts_with(b"NTLMSSP\0") {
        return Ok(token.to_vec());
    }
    let mut r = Reader::new(one(one(token, &[0xa1])?, &[0x30])?);
    let mut challenge = None;
    while r.remaining() > 0 {
        let (tag, body) = tlv(&mut r)?;
        match tag {
            0xa0 => {
                let state = one(body, &[0x0a])?;
                if state != [1] {
                    return Err(Error::Security(
                        "SPNEGO did not accept NTLM negotiation".into(),
                    ));
                }
            }
            0xa1 => {
                if one(body, &[6])? != [0x2b, 6, 1, 4, 1, 0x82, 0x37, 2, 2, 0x0a] {
                    return Err(Error::Unsupported(
                        "SPNEGO selected a mechanism other than NTLM".into(),
                    ));
                }
            }
            0xa2 => {
                if challenge.is_some() {
                    return Err(invalid("duplicate SPNEGO challenge"));
                }
                challenge = Some(one(body, &[4])?.to_vec());
            }
            _ => {
                return Err(Error::Unsupported(
                    "SPNEGO mechanism-list MIC exchange".into(),
                ));
            }
        }
    }
    challenge.ok_or_else(|| invalid("missing SPNEGO challenge"))
}
pub fn credentials(user: &str, domain: &str, password: &str) -> Zeroizing<Vec<u8>> {
    let mut fields = context(0, &octet(&crypto::utf16(domain)));
    fields.extend(context(1, &octet(&crypto::utf16(user))));
    let pass = Zeroizing::new(crypto::utf16(password));
    fields.extend(context(2, &octet(&pass)));
    let password_credentials = Zeroizing::new(sequence(&fields));
    use zeroize::Zeroize;
    fields.zeroize();
    let mut value = context(0, &integer(1));
    value.extend(context(1, &octet(&password_credentials)));
    let result = Zeroizing::new(sequence(&value));
    value.zeroize();
    result
}
pub fn authenticate(
    stream: &mut Transport,
    user: &str,
    domain: &str,
    password: &str,
    host: &str,
) -> Result<()> {
    if user.is_empty() {
        return Err(Error::Security(
            "NLA requires a user name and password".into(),
        ));
    }
    let certificate = stream.certificate()?.to_vec();
    let public_key = stream.public_key()?;
    let initial = initial_spnego(&ntlm::negotiate());
    stream.write_all(&request(6, Some(&initial), None, None, None))?;
    let challenge = response(&read_message(stream)?)?;
    let token = challenge_token(
        challenge
            .token
            .as_deref()
            .ok_or_else(|| invalid("CredSSP omitted NTLM challenge"))?,
    )?;
    let mut auth = ntlm::authenticate(&token, user, domain, password, &certificate, host)?;
    let nonce: [u8; 32] = crypto::random()?;
    let client_hash = crypto::sha256(&[
        b"CredSSP Client-To-Server Binding Hash\0",
        &nonce,
        &public_key,
    ]);
    let proof = auth.security.seal(&client_hash);
    let token = spnego_response(&auth.message);
    stream.write_all(&request(
        challenge.version,
        Some(&token),
        None,
        Some(&proof),
        Some(&nonce),
    ))?;
    let reply = response(&read_message(stream)?)?;
    if reply.version != challenge.version {
        return Err(Error::Security(
            "CredSSP version changed during authentication".into(),
        ));
    }
    let expected = crypto::sha256(&[
        b"CredSSP Server-To-Client Binding Hash\0",
        &nonce,
        &public_key,
    ]);
    let actual = auth.security.unseal(
        reply
            .key
            .as_deref()
            .ok_or_else(|| invalid("missing CredSSP server binding proof"))?,
    )?;
    if !crypto::equal(&expected, &actual) {
        return Err(Error::Security(
            "CredSSP TLS public-key binding mismatch; credentials were not delegated".into(),
        ));
    }
    let credentials = credentials(user, domain, password);
    let sealed = auth.security.seal(&credentials);
    stream.write_all(&request(challenge.version, None, Some(&sealed), None, None))?;
    stream.flush()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn field(packet: &[u8], tag: u8) -> Vec<u8> {
        let mut r = Reader::new(one(packet, &[0x30]).unwrap());
        while r.remaining() != 0 {
            let (t, b) = tlv(&mut r).unwrap();
            if t == tag {
                return one(b, &[4]).unwrap().to_vec();
            }
        }
        panic!("missing test field");
    }
    #[test]
    fn tls_credssp_exchange_and_binding_rejection() {
        use rustls::{ServerConfig, ServerConnection, StreamOwned, pki_types::PrivatePkcs8KeyDer};
        use std::{
            net::{TcpListener, TcpStream},
            sync::Arc,
            thread,
            time::Duration,
        };
        for bad_binding in [false, true] {
            let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let der = certificate.cert.der().to_vec();
            let pin = crypto::sha256(&[&der]);
            let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
            let public_key = cert.public_key().subject_public_key.data.to_vec();
            let config = ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.cert.der().clone()],
                PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der()).into(),
            )
            .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let peer = thread::spawn(move || {
                let (s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut tls = StreamOwned::new(ServerConnection::new(Arc::new(config)).unwrap(), s);
                let initial = response(&read_message(&mut tls).unwrap()).unwrap();
                assert!(initial.token.unwrap().windows(8).any(|s| s == b"NTLMSSP\0"));
                let mut target = vec![];
                target.extend([7, 0, 8, 0]);
                target.extend(0x01d0000000000000u64.to_le_bytes());
                target.extend([0; 4]);
                let mut challenge = b"NTLMSSP\0".to_vec();
                challenge.extend(2u32.to_le_bytes());
                challenge.extend([0; 8]);
                challenge.extend(0xe2888235u32.to_le_bytes());
                challenge.extend([0x55; 8]);
                challenge.extend([0; 8]);
                challenge.extend((target.len() as u16).to_le_bytes());
                challenge.extend((target.len() as u16).to_le_bytes());
                challenge.extend(56u32.to_le_bytes());
                challenge.extend([6, 1, 0xb1, 0x1d, 0, 0, 0, 15]);
                challenge.extend(target);
                tls.write_all(&request(6, Some(&challenge), None, None, None))
                    .unwrap();
                tls.flush().unwrap();
                let packet = read_message(&mut tls).unwrap();
                let auth = response(&packet).unwrap();
                let ntlm = challenge_token(&auth.token.unwrap()).unwrap();
                let mut security =
                    crate::ntlm::accept_for_test(&challenge, &ntlm, "Password", "User", "Domain");
                let nonce = field(&packet, 0xa5);
                let client = security.unseal(&auth.key.unwrap()).unwrap();
                assert_eq!(
                    client,
                    crypto::sha256(&[
                        b"CredSSP Client-To-Server Binding Hash\0",
                        &nonce,
                        &public_key
                    ])
                );
                let expected = crypto::sha256(&[
                    b"CredSSP Server-To-Client Binding Hash\0",
                    &nonce,
                    &public_key,
                ]);
                let proof = security.seal(if bad_binding { &[0; 32] } else { &expected });
                tls.write_all(&request(6, None, None, Some(&proof), None))
                    .unwrap();
                tls.flush().unwrap();
                if bad_binding {
                    let mut b = [0; 1];
                    assert!(!matches!(tls.read(&mut b),Ok(n)if n>0));
                } else {
                    let packet = read_message(&mut tls).unwrap();
                    let encrypted = field(&packet, 0xa2);
                    let clear = security.unseal(&encrypted).unwrap();
                    let mut r = Reader::new(one(&clear, &[0x30]).unwrap());
                    let (tag, kind) = tlv(&mut r).unwrap();
                    assert_eq!(tag, 0xa0);
                    assert_eq!(number(kind).unwrap(), 1);
                    let (tag, data) = tlv(&mut r).unwrap();
                    assert_eq!(tag, 0xa1);
                    let password = one(one(data, &[4]).unwrap(), &[0x30]).unwrap();
                    let mut r = Reader::new(password);
                    for (i, value) in ["Domain", "User", "Password"].into_iter().enumerate() {
                        let (tag, data) = tlv(&mut r).unwrap();
                        assert_eq!(tag, 0xa0 + i as u8);
                        assert_eq!(one(data, &[4]).unwrap(), crypto::utf16(value));
                    }
                }
            });
            let socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut tls = Transport::tls(socket, "localhost", Some(pin)).unwrap();
            let result = authenticate(&mut tls, "User", "Domain", "Password", "localhost");
            assert_eq!(result.is_err(), bad_binding);
            drop(tls);
            peer.join().unwrap();
        }
    }
    #[test]
    fn ts_request_roundtrip() {
        let p = request(6, Some(b"token"), None, Some(b"proof"), Some(&[7; 32]));
        let r = response(&p).unwrap();
        assert_eq!(r.version, 6);
        assert_eq!(r.token.unwrap(), b"token");
        assert_eq!(r.key.unwrap(), b"proof");
        for n in 0..p.len() {
            assert!(response(&p[..n]).is_err());
        }
    }
    #[test]
    fn downgrades_rejected() {
        assert!(response(&request(4, None, None, None, None)).is_err());
    }
    #[test]
    fn status_errors() {
        let mut fields = context(0, &integer(6));
        fields.extend(context(4, &integer(0xc000006d)));
        assert!(
            response(&sequence(&fields))
                .unwrap_err()
                .to_string()
                .contains("c000006d")
        );
    }
    #[test]
    fn spnego_challenge() {
        let token = b"NTLMSSP\0challenge";
        assert_eq!(challenge_token(&spnego_response(token)).unwrap(), token);
        assert!(challenge_token(&[0xa1, 0]).is_err());
    }
}
