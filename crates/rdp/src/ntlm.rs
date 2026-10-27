//! NTLMv2 messages and connection-oriented signing/sealing for CredSSP.
//! Deliberately excludes NTLMv1, anonymous authentication and weak session keys.
use crate::{
    Error, Result,
    crypto::{self, Rc4},
    invalid,
    wire::{Reader, u16le, u32le},
};
use md4::{Digest, Md4};
use zeroize::{Zeroize, Zeroizing};
const FLAGS: u32 = 0xe2888235;
const KEY_EXCH: u32 = 0x40000000;
const VERSION: [u8; 8] = [6, 1, 0xb1, 0x1d, 0, 0, 0, 15];
pub fn negotiate() -> Vec<u8> {
    let mut p = b"NTLMSSP\0".to_vec();
    u32le(&mut p, 1);
    u32le(&mut p, FLAGS);
    p.extend([0; 16]);
    p.extend(VERSION);
    p
}
fn field(packet: &[u8], offset: usize) -> Result<&[u8]> {
    let mut r = Reader::new(
        packet
            .get(offset..)
            .ok_or_else(|| invalid("NTLM buffer offset"))?,
    );
    let n = r.u16()? as usize;
    let max = r.u16()? as usize;
    let start = r.u32()? as usize;
    if max < n {
        return Err(invalid("NTLM buffer capacity"));
    }
    packet
        .get(
            start
                ..start
                    .checked_add(n)
                    .ok_or_else(|| invalid("NTLM offset overflow"))?,
        )
        .ok_or_else(|| invalid("NTLM buffer range"))
}
fn security_buffer(
    out: &mut Vec<u8>,
    payload: &mut Vec<u8>,
    base: usize,
    data: &[u8],
) -> Result<()> {
    let n = u16::try_from(data.len()).map_err(|_| invalid("NTLM field too long"))?;
    u16le(out, n);
    u16le(out, n);
    u32le(out, (base + payload.len()) as u32);
    payload.extend(data);
    Ok(())
}
pub struct Authentication {
    pub message: Vec<u8>,
    pub security: SessionSecurity,
}
pub fn authenticate(
    challenge: &[u8],
    user: &str,
    domain: &str,
    password: &str,
    certificate: &[u8],
    host: &str,
) -> Result<Authentication> {
    authenticate_with(
        challenge,
        user,
        domain,
        password,
        certificate,
        host,
        crypto::random()?,
        crypto::random()?,
    )
}
#[allow(clippy::too_many_arguments)]
fn authenticate_with(
    challenge: &[u8],
    user: &str,
    domain: &str,
    password: &str,
    certificate: &[u8],
    host: &str,
    client: [u8; 8],
    exported: [u8; 16],
) -> Result<Authentication> {
    let mut r = Reader::new(challenge);
    if r.take(8)? != b"NTLMSSP\0" || r.u32()? != 2 {
        return Err(invalid("NTLM challenge signature/type"));
    }
    r.take(8)?;
    let flags = r.u32()? & FLAGS;
    let server = r.take(8)?;
    r.take(8)?;
    let required = 0x20888231; // Unicode, sign/seal, NTLM, always-sign, ESS, target-info, 128-bit.
    if flags & required != required {
        return Err(Error::Security(
            "server did not negotiate NTLMv2 with 128-bit signing and sealing".into(),
        ));
    }
    let mut pairs = Reader::new(field(challenge, 40)?);
    let mut target = vec![];
    let mut timestamp = None;
    let mut av_flags = 2u32;
    let mut seen = std::collections::BTreeSet::new();
    let mut ended = false;
    while pairs.remaining() > 0 {
        let id = pairs.u16()?;
        let n = pairs.u16()? as usize;
        let value = pairs.take(n)?;
        if !seen.insert(id) {
            return Err(invalid("duplicate NTLM target-info AV pair"));
        }
        match id {
            0 => {
                if n != 0 || pairs.remaining() != 0 {
                    return Err(invalid("NTLM target-info terminator"));
                }
                ended = true;
                break;
            }
            6 => {
                if n != 4 {
                    return Err(invalid("NTLM AV flags"));
                }
                av_flags |= u32::from_le_bytes(value.try_into().unwrap());
            }
            7 => {
                if n != 8 {
                    return Err(invalid("NTLM AV timestamp"));
                }
                timestamp = Some(value.to_vec());
                le_pair(&mut target, id, value);
            }
            9 | 10 => {}
            _ => le_pair(&mut target, id, value),
        }
    }
    if !ended {
        return Err(invalid("missing NTLM target-info terminator"));
    }
    le_pair(&mut target, 6, &av_flags.to_le_bytes()); // Authenticate MIC is present.
    le_pair(&mut target, 9, &crypto::utf16(&format!("TERMSRV/{host}")));
    // RFC 5929 tls-server-end-point: SHA256 for the certificate algorithms
    // accepted by our TLS stack, including SHA1-signed legacy certificates.
    let (_, cert) = x509_parser::parse_x509_certificate(certificate)
        .map_err(|_| invalid("channel-binding certificate"))?;
    let mut algorithm = cert.signature_algorithm.algorithm.to_id_string();
    if algorithm == "1.2.840.113549.1.1.10" {
        use x509_parser::signature_algorithm::SignatureAlgorithm;
        match SignatureAlgorithm::try_from(&cert.signature_algorithm)
            .map_err(|_| invalid("RSA-PSS certificate signature parameters"))?
        {
            SignatureAlgorithm::RSASSA_PSS(params) => {
                algorithm = params.hash_algorithm_oid().to_id_string()
            }
            _ => return Err(invalid("RSA-PSS certificate signature algorithm")),
        }
    }
    let digest = match algorithm.as_str() {
        "1.2.840.113549.1.1.12" | "1.2.840.10045.4.3.3" | "2.16.840.1.101.3.4.2.2" => {
            sha2::Sha384::digest(certificate).to_vec()
        }
        "1.2.840.113549.1.1.13" | "1.2.840.10045.4.3.4" | "2.16.840.1.101.3.4.2.3" => {
            sha2::Sha512::digest(certificate).to_vec()
        }
        _ => crypto::sha256(&[certificate]).to_vec(),
    };
    let mut application = b"tls-server-end-point:".to_vec();
    application.extend(digest);
    let mut bindings = vec![0; 16];
    u32le(&mut bindings, application.len() as u32);
    bindings.extend(application);
    le_pair(&mut target, 10, &crypto::md5(&[&bindings]));
    le_pair(&mut target, 0, &[]);
    let now = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("system clock before Unix epoch"))?
        .as_nanos()
        / 100
        + 116444736000000000) as u64;
    let has_timestamp = timestamp.is_some();
    let time = timestamp.unwrap_or_else(|| now.to_le_bytes().to_vec());
    let password_utf16 = Zeroizing::new(crypto::utf16(password));
    let nt_hash = Zeroizing::new(Md4::digest(&password_utf16).to_vec());
    let response_key = Zeroizing::new(crypto::hmac_md5(
        &nt_hash,
        &[&crypto::utf16(&format!("{}{domain}", user.to_uppercase()))],
    ));
    let mut blob = vec![1, 1, 0, 0, 0, 0, 0, 0];
    blob.extend(time);
    blob.extend(client);
    blob.extend([0; 4]);
    blob.extend(target);
    blob.extend([0; 4]);
    let proof = crypto::hmac_md5(response_key.as_ref(), &[server, &blob]);
    let mut response = proof.to_vec();
    response.extend(blob);
    let base_key = Zeroizing::new(crypto::hmac_md5(response_key.as_ref(), &[&proof]));
    let mut session_key = Zeroizing::new(if flags & KEY_EXCH != 0 {
        exported
    } else {
        *base_key
    });
    let encrypted_key = if flags & KEY_EXCH != 0 {
        let mut key = session_key.to_vec();
        Rc4::new(base_key.as_ref()).apply(&mut key);
        key
    } else {
        vec![]
    };
    let lm = if has_timestamp {
        vec![0; 24]
    } else {
        let mut v = crypto::hmac_md5(response_key.as_ref(), &[server, &client]).to_vec();
        v.extend(client);
        v
    };
    let with_version = flags & 0x02000000 != 0;
    let header_len = 64 + if with_version { 8 } else { 0 } + 16;
    let mut out = b"NTLMSSP\0".to_vec();
    u32le(&mut out, 3);
    let mut payload = vec![];
    for data in [
        &lm[..],
        &response,
        &crypto::utf16(domain),
        &crypto::utf16(user),
        &crypto::utf16("rumstsc"),
        &encrypted_key,
    ] {
        security_buffer(&mut out, &mut payload, header_len, data)?;
    }
    u32le(&mut out, flags);
    if with_version {
        out.extend(VERSION);
    }
    let mic_offset = out.len();
    out.extend([0; 16]);
    out.extend(payload);
    let mic = crypto::hmac_md5(session_key.as_ref(), &[&negotiate(), challenge, &out]);
    out[mic_offset..mic_offset + 16].copy_from_slice(&mic);
    let security = SessionSecurity::new(&session_key, flags);
    session_key.zeroize();
    Ok(Authentication {
        message: out,
        security,
    })
}
fn le_pair(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    u16le(out, id);
    u16le(out, data.len() as u16);
    out.extend(data);
}
struct Direction {
    sign: [u8; 16],
    seal: Rc4,
    sequence: u32,
    key_exchange: bool,
}
impl Direction {
    fn new(key: &[u8], direction: &str, key_exchange: bool) -> Self {
        let sign = crypto::md5(&[
            key,
            format!("session key to {direction} signing key magic constant\0").as_bytes(),
        ]);
        let seal = crypto::md5(&[
            key,
            format!("session key to {direction} sealing key magic constant\0").as_bytes(),
        ]);
        Self {
            sign,
            seal: Rc4::new(&seal),
            sequence: 0,
            key_exchange,
        }
    }
    fn signature(&mut self, message: &[u8]) -> [u8; 16] {
        let mut sig = [0; 16];
        sig[..4].copy_from_slice(&1u32.to_le_bytes());
        sig[12..].copy_from_slice(&self.sequence.to_le_bytes());
        let checksum = crypto::hmac_md5(&self.sign, &[&self.sequence.to_le_bytes(), message]);
        sig[4..12].copy_from_slice(&checksum[..8]);
        if self.key_exchange {
            self.seal.apply(&mut sig[4..12]);
        }
        self.sequence = self.sequence.wrapping_add(1);
        sig
    }
}
impl Drop for Direction {
    fn drop(&mut self) {
        self.sign.zeroize();
    }
}
pub struct SessionSecurity {
    send: Direction,
    receive: Direction,
}
#[cfg(test)]
pub(crate) fn accept_for_test(
    challenge: &[u8],
    authenticate: &[u8],
    password: &str,
    user: &str,
    domain: &str,
) -> SessionSecurity {
    assert_eq!(field(authenticate, 28).unwrap(), crypto::utf16(domain));
    assert_eq!(field(authenticate, 36).unwrap(), crypto::utf16(user));
    let response = field(authenticate, 20).unwrap();
    let hash = Md4::digest(crypto::utf16(password));
    let response_key = crypto::hmac_md5(
        &hash,
        &[&crypto::utf16(&format!("{}{domain}", user.to_uppercase()))],
    );
    let proof = crypto::hmac_md5(&response_key, &[&challenge[24..32], &response[16..]]);
    assert_eq!(&response[..16], &proof);
    let key = crypto::hmac_md5(&response_key, &[&proof]);
    let mut exported = field(authenticate, 52).unwrap().to_vec();
    Rc4::new(&key).apply(&mut exported);
    let exported: [u8; 16] = exported.try_into().unwrap();
    let mut unsigned = authenticate.to_vec();
    let mic = unsigned[72..88].to_vec();
    unsigned[72..88].fill(0);
    assert_eq!(
        mic,
        crypto::hmac_md5(&exported, &[&negotiate(), challenge, &unsigned])
    );
    let mut security = SessionSecurity::new(&exported, FLAGS);
    std::mem::swap(&mut security.send, &mut security.receive);
    security
}
impl SessionSecurity {
    pub fn new(key: &[u8; 16], flags: u32) -> Self {
        Self {
            send: Direction::new(key, "client-to-server", flags & KEY_EXCH != 0),
            receive: Direction::new(key, "server-to-client", flags & KEY_EXCH != 0),
        }
    }
    pub fn seal(&mut self, message: &[u8]) -> Vec<u8> {
        let mut encrypted = message.to_vec();
        self.send.seal.apply(&mut encrypted);
        let mut result = self.send.signature(message).to_vec();
        result.extend(encrypted);
        result
    }
    pub fn unseal(&mut self, message: &[u8]) -> Result<Vec<u8>> {
        if message.len() < 16 {
            return Err(invalid("short NTLM sealed message"));
        }
        let mut plain = message[16..].to_vec();
        self.receive.seal.apply(&mut plain);
        let expected = self.receive.signature(&plain);
        if !crypto::equal(&expected, &message[..16]) {
            plain.zeroize();
            return Err(Error::Security("NTLM message signature mismatch".into()));
        }
        Ok(plain)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microsoft_seal_vector() {
        // MS-NLMP 4.2.3.4: explicit signing/sealing keys, no KEY_EXCH.
        let mut client = SessionSecurity {
            send: Direction {
                sign: [
                    0x60, 0xe7, 0x99, 0xbe, 0x5c, 0x72, 0xfc, 0x92, 0x92, 0x2a, 0xe8, 0xeb, 0xe9,
                    0x61, 0xfb, 0x8d,
                ],
                seal: Rc4::new(&[
                    0x04, 0xdd, 0x7f, 0x01, 0x4d, 0x85, 0x04, 0xd2, 0x65, 0xa2, 0x5c, 0xc8, 0x6a,
                    0x3a, 0x7c, 0x06,
                ]),
                sequence: 0,
                key_exchange: false,
            },
            receive: Direction::new(&[0; 16], "server-to-client", false),
        };
        assert_eq!(
            client.seal(&crypto::utf16("Plaintext")),
            [
                0x01, 0, 0, 0, 0xff, 0x2a, 0xeb, 0x52, 0xf6, 0x81, 0x79, 0x3a, 0, 0, 0, 0, 0xa0,
                0x23, 0x72, 0xf6, 0x53, 0x02, 0x73, 0xf3, 0xaa, 0x1e, 0xb9, 0x01, 0x90, 0xce, 0x52,
                0, 0xc9, 0x9d
            ]
        );
    }
    #[test]
    fn ntlmv2_response_key_vector() {
        let nt = Md4::digest(crypto::utf16("Password"));
        assert_eq!(
            crypto::hmac_md5(&nt, &[&crypto::utf16("USERDomain")]),
            [
                0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0,
                0x2e, 0x3f
            ]
        );
    }
    #[test]
    fn sealing_rejects_tampering_and_replay() {
        let key = [0x55; 16];
        let mut client = SessionSecurity::new(&key, KEY_EXCH);
        let mut server = SessionSecurity::new(&key, KEY_EXCH);
        std::mem::swap(&mut server.send, &mut server.receive);
        let p = client.seal(b"binding");
        assert_eq!(server.unseal(&p).unwrap(), b"binding");
        assert!(server.unseal(&p).is_err());
        let p = server.seal(b"server proof");
        assert_eq!(client.unseal(&p).unwrap(), b"server proof");
        let mut p = server.seal(b"secret");
        p[16] ^= 1;
        assert!(client.unseal(&p).is_err());
    }
    #[test]
    fn challenge_bounds() {
        for n in 0..56 {
            assert!(
                authenticate(&vec![0; n], "user", "domain", "password", &[], "server").is_err()
            );
        }
    }
}
