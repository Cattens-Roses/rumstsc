//! Small protocol-specific helpers. Hash primitives come from RustCrypto.
use crate::{Result, invalid};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use rand::RngCore;
use zeroize::Zeroize;
pub fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
pub fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = sha1::Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = sha2::Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}
pub fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Hmac::<Md5>::new_from_slice(key).expect("HMAC accepts any key size");
    for p in parts {
        h.update(p);
    }
    h.finalize().into_bytes().into()
}
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0; N];
    rand::rngs::OsRng
        .try_fill_bytes(&mut b)
        .map_err(|_| invalid("operating system random source unavailable"))?;
    Ok(b)
}
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(a.ct_eq(b))
}
pub fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
pub fn fingerprint(data: &[u8]) -> String {
    sha256(&[data]).iter().map(|b| format!("{b:02x}")).collect()
}
pub fn parse_pin(s: &str) -> Result<Option<[u8; 32]>> {
    let s: String = s.trim().chars().filter(|&c| c != ':').collect();
    if s.is_empty() {
        return Ok(None);
    }
    if s.len() != 64 || !s.is_ascii() {
        return Err(invalid(
            "certificate SHA-256 fingerprint must contain 64 hex digits",
        ));
    }
    let mut pin = [0; 32];
    for (i, p) in pin.iter_mut().enumerate() {
        *p = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| invalid("certificate fingerprint is not hexadecimal"))?;
    }
    Ok(Some(pin))
}
pub struct Rc4 {
    state: [u8; 256],
    i: u8,
    j: u8,
}
impl Rc4 {
    pub fn new(key: &[u8]) -> Self {
        assert!(!key.is_empty());
        let mut state = [0; 256];
        for (i, b) in state.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
            state.swap(i, j as usize);
        }
        Self { state, i: 0, j: 0 }
    }
    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.state[self.i as usize]);
            self.state.swap(self.i as usize, self.j as usize);
            *b ^= self.state
                [self.state[self.i as usize].wrapping_add(self.state[self.j as usize]) as usize];
        }
    }
}
impl Drop for Rc4 {
    fn drop(&mut self) {
        self.state.zeroize();
        self.i.zeroize();
        self.j.zeroize();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rc4_known_vector() {
        let mut p = b"Plaintext".to_vec();
        Rc4::new(b"Key").apply(&mut p);
        assert_eq!(p, [0xbb, 0xf3, 0x16, 0xe8, 0xd9, 0x40, 0xaf, 0x0a, 0xd3]);
    }
    #[test]
    fn pins() {
        assert_eq!(
            parse_pin(&"AB:".repeat(31).to_owned().add("AB")).unwrap(),
            Some([0xab; 32])
        );
        assert!(parse_pin("ab").is_err());
    }
    use std::ops::Add;
}
