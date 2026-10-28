//! Independent RDP, CredSSP/NTLMv2, licensing and bitmap wire implementations.
//! Uses maintained TLS/crypto and system H.264 primitives; no external RDP client.
#![forbid(unsafe_code)]
mod autodetect;
pub mod bitmap;
mod bulk;
mod cache;
mod clear;
pub mod clipboard;
pub mod credssp;
pub mod crypto;
mod dvc;
mod gfx;
mod graphics;
pub mod legacy;
pub mod licensing;
pub mod mcs;
pub mod negotiation;
mod nsc;
pub mod ntlm;
pub mod orders;
mod planar;
pub mod pointer;
mod progressive;
pub mod rle;
pub mod session;
pub mod trace;
pub mod transport;
pub mod wire;
mod zgfx;

use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Protocol(String),
    Unsupported(String),
    Security(String),
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "Network: {e}"),
            Self::Protocol(e) => write!(f, "Invalid RDP packet: {e}"),
            Self::Unsupported(e) => write!(f, "Unsupported: {e}"),
            Self::Security(e) => write!(f, "Security: {e}"),
            Self::Cancelled => write!(f, "Disconnected"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn invalid(message: &str) -> Error {
    Error::Protocol(message.into())
}
