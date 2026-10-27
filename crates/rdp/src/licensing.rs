//! MS-RDPELE new-license exchange. Licenses are verified in memory, not persisted.
use crate::{
    Error, Result,
    crypto::{self, Rc4},
    invalid, legacy,
    wire::{Reader, u16le, u32le},
};
use rsa::{BigUint, RsaPublicKey, pkcs1::DecodeRsaPublicKey, traits::PublicKeyParts};
use zeroize::{Zeroize, Zeroizing};
#[derive(Default)]
pub struct Licensing {
    key: Option<Keys>,
    pub complete: bool,
    requested: bool,
    challenged: bool,
}
struct Keys {
    encryption: [u8; 16],
    mac: [u8; 16],
}
impl Drop for Keys {
    fn drop(&mut self) {
        self.encryption.zeroize();
        self.mac.zeroize();
    }
}
fn blob<'a>(r: &mut Reader<'a>) -> Result<(u16, &'a [u8])> {
    let kind = r.u16()?;
    let n = r.u16()? as usize;
    Ok((kind, r.take(n)?))
}
fn put_blob(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    u16le(out, kind);
    u16le(out, data.len() as u16);
    out.extend(data);
}
fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut p = vec![kind, 3];
    u16le(&mut p, (body.len() + 4) as u16);
    p.extend(body);
    p
}
impl Keys {
    fn derive(premaster: &[u8], client: &[u8], server: &[u8]) -> Self {
        let salted = |secret: &[u8], label: &[u8], a: &[u8], b: &[u8]| {
            crypto::md5(&[secret, &crypto::sha1(&[label, secret, a, b])])
        };
        let mut master = Zeroizing::new(vec![]);
        for label in [b"A".as_slice(), b"BB", b"CCC"] {
            master.extend(salted(premaster, label, client, server));
        }
        let mut session = Zeroizing::new(vec![]);
        for label in [b"A".as_slice(), b"BB", b"CCC"] {
            session.extend(salted(&master, label, server, client));
        }
        Self {
            mac: session[..16].try_into().unwrap(),
            encryption: crypto::md5(&[&session[16..32], client, server]),
        }
    }
    fn crypt(&self, data: &[u8]) -> Vec<u8> {
        let mut p = data.to_vec();
        Rc4::new(&self.encryption).apply(&mut p);
        p
    }
    fn mac(&self, data: &[u8]) -> [u8; 16] {
        crypto::md5(&[
            &self.mac,
            &[0x5c; 48],
            &crypto::sha1(&[
                &self.mac,
                &[0x36; 40],
                &(data.len() as u32).to_le_bytes(),
                data,
            ]),
        ])
    }
}
impl Licensing {
    pub fn process(
        &mut self,
        packet: &[u8],
        username: &str,
        default_cert: &[u8],
        tls_cert: Option<&[u8]>,
    ) -> Result<Option<Vec<u8>>> {
        if self.complete {
            return Err(invalid("licensing PDU after completion"));
        }
        let mut r = Reader::new(packet);
        let kind = r.u8()?;
        let version = r.u8()? & 15;
        let n = r.u16()? as usize;
        if ![2, 3].contains(&version) || n != packet.len() {
            return Err(invalid("license preamble"));
        }
        let reply = match kind {
            0xff => {
                let code = r.u32()?;
                let transition = r.u32()?;
                let (_, error) = blob(&mut r)?;
                if code != 7 || transition != 2 || !error.is_empty() {
                    return Err(Error::Security(format!(
                        "RDP licensing rejected (code {code}, transition {transition})"
                    )));
                }
                self.complete = true;
                None
            }
            1 => {
                if self.requested {
                    return Err(invalid("duplicate license request"));
                }
                self.requested = true;
                let server = r.take(32)?;
                r.u32()?;
                for _ in 0..2 {
                    let n = r.u32()? as usize;
                    r.take(n)?;
                }
                let (_, algorithms) = blob(&mut r)?;
                if !algorithms
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|v| v == &[1, 0, 0, 0])
                {
                    return Err(Error::Unsupported(
                        "licensing key-exchange algorithm".into(),
                    ));
                }
                let (_, certificate) = blob(&mut r)?;
                let count = r.u32()?;
                if count > 16 {
                    return Err(invalid("license scope count"));
                }
                for _ in 0..count {
                    blob(&mut r)?;
                }
                let certificate = if certificate.is_empty() {
                    default_cert
                } else {
                    certificate
                };
                let public = if certificate.is_empty() {
                    let (_, cert) = x509_parser::parse_x509_certificate(
                        tls_cert.ok_or_else(|| invalid("license server omitted its public key"))?,
                    )
                    .map_err(|_| invalid("licensing TLS certificate"))?;
                    RsaPublicKey::from_pkcs1_der(&cert.public_key().subject_public_key.data)
                        .map_err(|_| invalid("licensing RSA public key"))?
                } else {
                    legacy::public_key(certificate)?
                };
                let client = crypto::random::<32>()?;
                let premaster = Zeroizing::new(crypto::random::<48>()?);
                let mut encrypted = BigUint::from_bytes_le(premaster.as_ref())
                    .modpow(public.e(), public.n())
                    .to_bytes_le();
                encrypted.resize(public.size() + 8, 0);
                self.key = Some(Keys::derive(premaster.as_ref(), &client, server));
                let mut body = vec![];
                u32le(&mut body, 1);
                u32le(&mut body, 0x04010000);
                body.extend(client);
                put_blob(&mut body, 2, &encrypted);
                let username = if username.is_ascii() && !username.contains('\0') {
                    username
                } else {
                    "rumstsc"
                };
                let mut username = username.as_bytes().to_vec();
                username.push(0);
                put_blob(&mut body, 0x0f, &username);
                put_blob(&mut body, 0x10, b"rumstsc\0");
                Some(message(0x13, &body))
            }
            2 => {
                if self.challenged {
                    return Err(invalid("duplicate licensing platform challenge"));
                }
                self.challenged = true;
                let keys = self
                    .key
                    .as_ref()
                    .ok_or_else(|| invalid("license challenge before request"))?;
                r.u32()?;
                let (_, encrypted) = blob(&mut r)?;
                let signature = r.take(16)?;
                let challenge = keys.crypt(encrypted);
                if !crypto::equal(signature, &keys.mac(&challenge)) {
                    return Err(Error::Security(
                        "license platform challenge MAC mismatch".into(),
                    ));
                }
                let mut response = vec![];
                for v in [0x0100, 0xff00, 3, challenge.len() as u16] {
                    u16le(&mut response, v);
                }
                response.extend(challenge);
                let mut hardware = vec![];
                u32le(&mut hardware, 0x04010000);
                hardware.extend(crypto::md5(&[b"rumstsc"]));
                let mac = keys.mac(&[response.as_slice(), hardware.as_slice()].concat());
                let mut body = vec![];
                put_blob(&mut body, 9, &keys.crypt(&response));
                put_blob(&mut body, 9, &keys.crypt(&hardware));
                body.extend(mac);
                Some(message(0x15, &body))
            }
            3 | 4 => {
                let keys = self
                    .key
                    .as_ref()
                    .ok_or_else(|| invalid("license issuance before request"))?;
                let (_, encrypted) = blob(&mut r)?;
                let signature = r.take(16)?;
                let plain = Zeroizing::new(keys.crypt(encrypted));
                if !crypto::equal(signature, &keys.mac(&plain)) {
                    return Err(Error::Security("issued license MAC mismatch".into()));
                }
                let mut info = Reader::new(&plain);
                info.u32()?;
                for _ in 0..4 {
                    let n = info.u32()? as usize;
                    info.take(n)?;
                }
                if info.remaining() != 0 {
                    return Err(invalid("new license information length"));
                }
                self.complete = true;
                None
            }
            _ => return Err(Error::Unsupported(format!("licensing message {kind}"))),
        };
        if r.remaining() != 0 {
            return Err(invalid("license PDU trailing data"));
        }
        Ok(reply)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_client_alert() {
        let mut license = Licensing::default();
        assert!(
            license
                .process(
                    &[255, 3, 16, 0, 7, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0],
                    "",
                    &[],
                    None
                )
                .unwrap()
                .is_none()
        );
        assert!(license.complete);
    }
    #[test]
    fn invalid_license_alerts() {
        for n in 0..16 {
            assert!(
                Licensing::default()
                    .process(&[0; 16][..n], "", &[], None)
                    .is_err()
            );
        }
        assert!(
            Licensing::default()
                .process(
                    &[255, 3, 16, 0, 1, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0],
                    "",
                    &[],
                    None
                )
                .is_err()
        );
    }
}
