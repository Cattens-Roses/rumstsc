//! Standard RDP security: RSA random exchange, RC4, MAC and 4096-packet rekey.
//! This compatibility mode requires an explicit server certificate pin.
use crate::{
    Error, Result,
    crypto::{self, Rc4},
    invalid,
    wire::{Reader, u16le, u32le},
};
use rsa::{BigUint, RsaPublicKey, pkcs1::DecodeRsaPublicKey, traits::PublicKeyParts};
use zeroize::{Zeroize, Zeroizing};
pub struct Legacy {
    mac: Vec<u8>,
    encrypt: Cipher,
    decrypt: Cipher,
    level: u32,
}
struct Cipher {
    initial: Vec<u8>,
    key: Vec<u8>,
    cipher: Rc4,
    count: u32,
    total: u32,
    method: u32,
}
fn reduce(key: &[u8], method: u32) -> Vec<u8> {
    let mut key = key[..if method == 2 { 16 } else { 8 }].to_vec();
    if method == 1 {
        key[..3].copy_from_slice(&[0xd1, 0x26, 0x9e]);
    } else if method == 8 {
        key[0] = 0xd1;
    }
    key
}
impl Cipher {
    fn new(key: Vec<u8>, method: u32) -> Self {
        Self {
            initial: key.clone(),
            cipher: Rc4::new(&key),
            key,
            count: 0,
            total: 0,
            method,
        }
    }
    fn apply(&mut self, data: &mut [u8]) {
        if self.count == 4096 {
            let sha = crypto::sha1(&[&self.initial, &[0x36; 40], &self.key]);
            let temp = Zeroizing::new(crypto::md5(&[&self.initial, &[0x5c; 48], &sha]));
            let n = self.key.len();
            let mut next = Zeroizing::new(temp[..n].to_vec());
            Rc4::new(&next.clone()).apply(&mut next);
            self.key.zeroize();
            self.key = reduce(&next, self.method);
            self.cipher = Rc4::new(&self.key);
            self.count = 0;
        }
        self.cipher.apply(data);
        self.count += 1;
        self.total = self.total.wrapping_add(1);
    }
}
impl Drop for Cipher {
    fn drop(&mut self) {
        self.key.zeroize();
        self.initial.zeroize();
    }
}
impl Drop for Legacy {
    fn drop(&mut self) {
        self.mac.zeroize();
    }
}
impl Legacy {
    pub fn exchange(
        method: u32,
        level: u32,
        server_random: &[u8],
        certificate: &[u8],
        pin: Option<[u8; 32]>,
    ) -> Result<(Self, Vec<u8>)> {
        if ![1, 2, 8].contains(&method) || !(1..=3).contains(&level) {
            return Err(Error::Unsupported(
                "legacy encryption method/level (FIPS is not supported)".into(),
            ));
        }
        if server_random.len() != 32 {
            return Err(invalid("legacy encryption server random length"));
        }
        let fingerprint = crypto::fingerprint(certificate);
        if !pin.is_some_and(|pin| crypto::equal(&pin, &crypto::sha256(&[certificate]))) {
            return Err(Error::Security(format!(
                "legacy encryption certificate requires its SHA-256 pin: {fingerprint}; no credentials were sent"
            )));
        }
        let public = public_key(certificate)?;
        let client = Zeroizing::new(crypto::random::<32>()?);
        let modulus_len = public.size();
        let number = BigUint::from_bytes_le(client.as_ref());
        let mut encrypted = number.modpow(public.e(), public.n()).to_bytes_le();
        encrypted.resize(modulus_len + 8, 0);
        let mut exchange = vec![];
        u32le(&mut exchange, 1);
        u32le(&mut exchange, encrypted.len() as u32);
        exchange.extend(encrypted);
        Ok((
            Self::derive(client.as_ref(), server_random, method, level),
            exchange,
        ))
    }
    fn derive(client: &[u8], server: &[u8], method: u32, level: u32) -> Self {
        let mut premaster = Zeroizing::new(client[..24].to_vec());
        premaster.extend(&server[..24]);
        let salted = |secret: &[u8], label: &[u8]| {
            crypto::md5(&[secret, &crypto::sha1(&[label, secret, client, server])])
        };
        let mut master = Zeroizing::new(vec![]);
        for label in [b"A".as_slice(), b"BB", b"CCC"] {
            master.extend(salted(&premaster, label));
        }
        let mut blob = Zeroizing::new(vec![]);
        for label in [b"X".as_slice(), b"YY", b"ZZZ"] {
            blob.extend(salted(&master, label));
        }
        let mac = reduce(&blob[..16], method);
        let decrypt = reduce(&crypto::md5(&[&blob[16..32], client, server]), method);
        let encrypt = reduce(&crypto::md5(&[&blob[32..48], client, server]), method);
        Self {
            mac,
            encrypt: Cipher::new(encrypt, method),
            decrypt: Cipher::new(decrypt, method),
            level,
        }
    }
    fn mac(&self, data: &[u8], count: Option<u32>) -> [u8; 8] {
        let len = (data.len() as u32).to_le_bytes();
        let count = count.map(u32::to_le_bytes);
        let mut parts = vec![self.mac.as_slice(), &[0x36; 40], &len, data];
        if let Some(count) = &count {
            parts.push(count);
        }
        let sha = crypto::sha1(&parts);
        crypto::md5(&[&self.mac, &[0x5c; 48], &sha])[..8]
            .try_into()
            .unwrap()
    }
    pub fn protect(&mut self, flags: u16, body: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        u16le(&mut out, flags | 8);
        u16le(&mut out, 0);
        out.extend(self.mac(body, None));
        let mut encrypted = body.to_vec();
        self.encrypt.apply(&mut encrypted);
        out.extend(encrypted);
        out
    }
    pub fn unprotect(&mut self, data: &[u8]) -> Result<(u16, Vec<u8>)> {
        let mut r = Reader::new(data);
        let flags = r.u16()?;
        r.u16()?; // flagsHigh is reserved and MUST be ignored.
        if flags & 8 == 0 {
            if self.level != 1 && flags & 0x80 == 0 {
                return Err(Error::Security(
                    "unencrypted server data in an encrypted RDP session".into(),
                ));
            }
            return Ok((flags, r.rest().to_vec()));
        }
        let signature = r.take(8)?;
        let mut plain = r.rest().to_vec();
        let count = self.decrypt.total;
        self.decrypt.apply(&mut plain);
        if !crypto::equal(
            signature,
            &self.mac(
                &plain,
                if flags & 0x800 != 0 {
                    Some(count)
                } else {
                    None
                },
            ),
        ) {
            plain.zeroize();
            return Err(Error::Security("legacy RDP packet MAC mismatch".into()));
        }
        Ok((flags, plain))
    }
}
pub fn public_key(certificate: &[u8]) -> Result<RsaPublicKey> {
    let mut r = Reader::new(certificate);
    let version = r.u32()? & 0x7fffffff;
    let key = match version {
        1 => {
            if r.u32()? != 1 || r.u32()? != 1 || r.u16()? != 6 {
                return Err(invalid("legacy proprietary certificate algorithm"));
            }
            let n = r.u16()? as usize;
            let mut key = Reader::new(r.take(n)?);
            if key.u32()? != 0x31415352 {
                return Err(invalid("legacy RSA magic"));
            }
            let length = key.u32()? as usize;
            let bits = key.u32()? as usize;
            let data_len = key.u32()? as usize;
            let exponent = key.u32()?;
            if !(72..=520).contains(&length) || bits != (length - 8) * 8 || data_len != length - 9 {
                return Err(invalid("legacy RSA key lengths"));
            }
            let modulus = BigUint::from_bytes_le(key.take(length - 8)?);
            key.take(8)?;
            if key.remaining() != 0 {
                return Err(invalid("legacy RSA blob length"));
            }
            if r.u16()? != 8 {
                return Err(invalid("legacy signature blob type"));
            }
            let signature_len = r.u16()? as usize;
            r.take(signature_len)?;
            if r.remaining() != 0 {
                return Err(invalid("legacy certificate trailing bytes"));
            }
            RsaPublicKey::new(modulus, BigUint::from(exponent))
                .map_err(|_| invalid("legacy RSA public key"))?
        }
        2 => {
            let count = r.u32()?;
            if count == 0 || count > 32 {
                return Err(invalid("legacy certificate chain length"));
            }
            let mut leaf = &[][..];
            for _ in 0..count {
                let n = r.u32()? as usize;
                leaf = r.take(n)?;
            }
            let (_, cert) = x509_parser::parse_x509_certificate(leaf)
                .map_err(|_| invalid("legacy X.509 certificate"))?;
            RsaPublicKey::from_pkcs1_der(&cert.public_key().subject_public_key.data)
                .map_err(|_| invalid("legacy certificate RSA public key"))?
        }
        _ => return Err(invalid("legacy certificate version")),
    };
    if key.size() < 64 || key.size() > 512 {
        return Err(invalid("legacy RSA key outside supported size"));
    }
    Ok(key)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encryption_mac_and_rekey() {
        for method in [1, 2, 8] {
            let mut client = Legacy::derive(&[0x11; 32], &[0x22; 32], method, 3);
            let mut server = Legacy::derive(&[0x11; 32], &[0x22; 32], method, 3);
            std::mem::swap(&mut server.encrypt, &mut server.decrypt);
            for i in 0..4100u32 {
                let p = client.protect(0, &i.to_le_bytes());
                let (_, plain) = server.unprotect(&p).unwrap();
                assert_eq!(plain, i.to_le_bytes());
            }
            let mut p = server.protect(0, b"server");
            p[12] ^= 1;
            assert!(client.unprotect(&p).is_err());
        }
    }
    #[test]
    fn reduction() {
        assert_eq!(&reduce(&[0; 16], 1)[..3], &[0xd1, 0x26, 0x9e]);
        assert_eq!(reduce(&[0; 16], 8)[0], 0xd1);
        assert_eq!(reduce(&[0; 16], 2).len(), 16);
    }
}
