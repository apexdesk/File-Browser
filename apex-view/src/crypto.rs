//! The ciphers of encrypted PDFs: RC4 and AES (CBC), decrypting only.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

/// The RC4 stream cipher.
pub(crate) struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    pub(crate) fn new(key: &[u8]) -> Rc4 {
        let mut s = [0u8; 256];
        for (i, x) in s.iter_mut().enumerate() {
            *x = i as u8;
        }
        let mut j = 0u8;
        if !key.is_empty() {
            for i in 0..256 {
                j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
                s.swap(i, j as usize);
            }
        }
        Rc4 { s, i: 0, j: 0 }
    }

    pub(crate) fn apply(&mut self, data: &mut [u8]) {
        for b in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            *b ^= self.s[self.s[self.i as usize].wrapping_add(self.s[self.j as usize]) as usize];
        }
    }
}

/// AES with a 128, 192 or 256-bit key.
pub(crate) enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl Aes {
    pub(crate) fn new(key: &[u8]) -> Result<Aes, ()> {
        Ok(match key.len() {
            16 => Aes::A128(aes::Aes128::new(GenericArray::from_slice(key))),
            24 => Aes::A192(aes::Aes192::new(GenericArray::from_slice(key))),
            32 => Aes::A256(aes::Aes256::new(GenericArray::from_slice(key))),
            _ => return Err(()),
        })
    }

    pub(crate) fn decrypt(&self, b: &mut [u8]) {
        let b = GenericArray::from_mut_slice(b);
        match self {
            Aes::A128(c) => c.decrypt_block(b),
            Aes::A192(c) => c.decrypt_block(b),
            Aes::A256(c) => c.decrypt_block(b),
        }
    }

    pub(crate) fn encrypt(&self, b: &mut [u8]) {
        let b = GenericArray::from_mut_slice(b);
        match self {
            Aes::A128(c) => c.encrypt_block(b),
            Aes::A192(c) => c.encrypt_block(b),
            Aes::A256(c) => c.encrypt_block(b),
        }
    }
}

/// AES-CBC without padding (a trailing partial block is left out).
pub(crate) fn cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, ()> {
    let aes = Aes::new(key)?;
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv.get(..16).ok_or(())?);
    let mut out = Vec::with_capacity(data.len());
    for c in data.chunks_exact(16) {
        let mut b = [0u8; 16];
        b.copy_from_slice(c);
        aes.decrypt(&mut b);
        for (x, p) in b.iter_mut().zip(prev) {
            *x ^= p;
        }
        out.extend_from_slice(&b);
        prev.copy_from_slice(c);
    }
    Ok(out)
}
