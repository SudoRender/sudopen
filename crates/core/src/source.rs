//! Raw bytes of an opened file.
//!
//! Small files are read into memory. Larger ones are memory-mapped, so opening
//! a multi-GB file costs the same as opening a tiny one: the OS only loads the
//! pages that are actually looked at.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

/// Files at or above this size are memory-mapped instead of read.
const MMAP_THRESHOLD: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf8Bom => "UTF-8 BOM",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
        }
    }
}

enum Storage {
    Owned(Vec<u8>),
    Mapped(Mmap),
}

pub struct Source {
    path: PathBuf,
    storage: Storage,
    /// Offset where the text starts (skips a UTF-8 BOM).
    start: usize,
    file_len: u64,
    pub encoding: Encoding,
}

impl Source {
    pub fn open(path: &Path) -> io::Result<Source> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "is a directory"));
        }
        let file_len = meta.len();
        let storage = if file_len < MMAP_THRESHOLD {
            let mut buf = Vec::with_capacity(file_len as usize);
            file.read_to_end(&mut buf)?;
            Storage::Owned(buf)
        } else {
            // SAFETY: if another process truncates the file while it is mapped,
            // touching the missing pages raises SIGBUS. Accepted for now (see
            // CLAUDE.md, "Known gaps").
            Storage::Mapped(unsafe { Mmap::map(&file)? })
        };
        Ok(Self::from_storage(path.to_path_buf(), storage, file_len))
    }

    /// Wraps bytes that did not come from disk (stdin, tests).
    pub fn from_bytes(path: PathBuf, bytes: Vec<u8>) -> Source {
        let len = bytes.len() as u64;
        Self::from_storage(path, Storage::Owned(bytes), len)
    }

    fn from_storage(path: PathBuf, storage: Storage, file_len: u64) -> Source {
        let raw: &[u8] = match &storage {
            Storage::Owned(v) => v,
            Storage::Mapped(m) => m,
        };
        let (encoding, start, decoded) = if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (Encoding::Utf8Bom, 3, None)
        } else if raw.starts_with(&[0xFF, 0xFE]) {
            (Encoding::Utf16Le, 0, Some(decode_utf16(&raw[2..], u16::from_le_bytes)))
        } else if raw.starts_with(&[0xFE, 0xFF]) {
            (Encoding::Utf16Be, 0, Some(decode_utf16(&raw[2..], u16::from_be_bytes)))
        } else {
            (Encoding::Utf8, 0, None)
        };
        let storage = match decoded {
            Some(text) => Storage::Owned(text.into_bytes()),
            None => storage,
        };
        Source { path, storage, start, file_len, encoding }
    }

    /// The text, always UTF-8 (possibly with invalid sequences).
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        let raw: &[u8] = match &self.storage {
            Storage::Owned(v) => v,
            Storage::Mapped(m) => m,
        };
        &raw[self.start..]
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.bytes().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }

    /// Size on disk, for display.
    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_mapped(&self) -> bool {
        matches!(self.storage, Storage::Mapped(_))
    }
}

fn decode_utf16(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> String {
    let units = bytes.as_chunks::<2>().0.iter().map(|c| to_u16([c[0], c[1]]));
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_utf8_bom() {
        let s = Source::from_bytes("a".into(), b"\xEF\xBB\xBFhi".to_vec());
        assert_eq!(s.bytes(), b"hi");
        assert_eq!(s.encoding, Encoding::Utf8Bom);
    }

    #[test]
    fn decodes_utf16le() {
        let s = Source::from_bytes("a".into(), vec![0xFF, 0xFE, b'h', 0, b'i', 0]);
        assert_eq!(s.bytes(), b"hi");
        assert_eq!(s.encoding, Encoding::Utf16Le);
    }
}
