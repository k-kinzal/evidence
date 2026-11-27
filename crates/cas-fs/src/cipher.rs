use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305,
    aead::{
        Aead, Payload,
        generic_array::GenericArray,
        stream::{DecryptorLE31, EncryptorLE31},
    },
};
use zeroize::Zeroizing;

use crate::{Hasher, ObjectId};

const STREAM_MAGIC: &[u8; 8] = b"EVOBJ001";
const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
const STREAM_NONCE: usize = 20;
const NONCE: usize = 24;

/// An opaque 32-byte key for authenticated object encryption.
#[derive(Clone)]
pub struct EncryptionKey(Arc<Zeroizing<[u8; 32]>>);

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptionKey").finish_non_exhaustive()
    }
}

impl EncryptionKey {
    /// Takes ownership of key bytes and clears them when the last clone is dropped.
    pub fn new(bytes: Zeroizing<[u8; 32]>) -> Self {
        Self(Arc::new(bytes))
    }

    /// Identifies which random key can open an encrypted record.
    pub fn id(&self) -> ObjectId {
        let mut hash = Hasher::new();
        hash.update(b"evidence-local-key-id/v1\0");
        hash.update(self.0.as_ref().as_ref());
        hash.finish()
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(GenericArray::from_slice(self.0.as_slice()))
    }

    /// Seals small metadata with a fresh nonce and authenticated context.
    pub fn seal(&self, context: &[u8], bytes: &[u8]) -> io::Result<Vec<u8>> {
        let nonce = random::<NONCE>()?;
        let cipher = self
            .cipher()
            .encrypt(
                GenericArray::from_slice(&nonce),
                Payload {
                    msg: bytes,
                    aad: context,
                },
            )
            .map_err(|_| damaged())?;
        let mut result = nonce.to_vec();
        result.extend_from_slice(&cipher);
        Ok(result)
    }

    /// Authenticates and opens metadata sealed with this key and context.
    pub fn open(&self, context: &[u8], bytes: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
        let (nonce, msg) = bytes.split_first_chunk::<NONCE>().ok_or_else(damaged)?;
        self.cipher()
            .decrypt(
                GenericArray::from_slice(nonce),
                Payload { msg, aad: context },
            )
            .map(Zeroizing::new)
            .map_err(|_| damaged())
    }

    /// Encrypts a stream with authenticated frames and returns the plaintext and ciphertext digests.
    pub fn encrypt_object(
        &self,
        mut reader: impl Read,
        mut writer: impl Write,
        context: &[u8],
    ) -> io::Result<(ObjectId, ObjectId)> {
        let nonce = random::<STREAM_NONCE>()?;
        let mut plain = Hasher::new();
        let mut encrypted = Hasher::new();
        for header in [STREAM_MAGIC.as_slice(), &nonce] {
            writer.write_all(header)?;
            encrypted.update(header);
        }
        let mut enc = EncryptorLE31::from_aead(self.cipher(), GenericArray::from_slice(&nonce));
        let mut window = Zeroizing::new(vec![0; CHUNK]);
        loop {
            let n = fill(&mut reader, &mut window)?;
            plain.update(&window[..n]);
            let payload = Payload {
                msg: &window[..n],
                aad: context,
            };
            if n == CHUNK {
                let record = enc.encrypt_next(payload).map_err(|_| damaged())?;
                writer.write_all(&record)?;
                encrypted.update(&record);
            } else {
                let record = enc.encrypt_last(payload).map_err(|_| damaged())?;
                writer.write_all(&record)?;
                encrypted.update(&record);
                return Ok((plain.finish(), encrypted.finish()));
            }
        }
    }

    /// Authenticates and decrypts a stream. Hold the output privately until this returns successfully.
    pub fn decrypt_object(
        &self,
        mut reader: impl Read,
        mut writer: impl Write,
        context: &[u8],
    ) -> io::Result<(ObjectId, ObjectId)> {
        let mut header = [0; STREAM_MAGIC.len() + STREAM_NONCE];
        reader.read_exact(&mut header)?;
        if !header.starts_with(STREAM_MAGIC) {
            return Err(damaged());
        }
        let nonce = GenericArray::from_slice(&header[STREAM_MAGIC.len()..]);
        let mut dec = DecryptorLE31::from_aead(self.cipher(), nonce);
        let mut plain = Hasher::new();
        let mut encrypted = Hasher::new();
        encrypted.update(&header);
        let mut window = vec![0; CHUNK + TAG];
        loop {
            let n = fill(&mut reader, &mut window)?;
            encrypted.update(&window[..n]);
            let payload = Payload {
                msg: &window[..n],
                aad: context,
            };
            if n == window.len() {
                let record = Zeroizing::new(dec.decrypt_next(payload).map_err(|_| damaged())?);
                writer.write_all(&record)?;
                plain.update(&record);
            } else {
                let record = Zeroizing::new(dec.decrypt_last(payload).map_err(|_| damaged())?);
                writer.write_all(&record)?;
                plain.update(&record);
                return Ok((plain.finish(), encrypted.finish()));
            }
        }
    }
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|_| io::Error::other("system randomness unavailable"))?;
    Ok(bytes)
}

fn fill(reader: &mut impl Read, window: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < window.len() {
        match reader.read(&mut window[n..]) {
            Ok(0) => break,
            Ok(written) => n += written,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn damaged() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "encrypted object authentication failed",
    )
}
