//! File loading and line indexing.

use std::io;
use std::path::Path;

use crate::trace;

pub enum Bytes {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

/// One side of the diff: the raw bytes plus an index of line start offsets.
pub struct FileData {
    pub bytes: Bytes,
    /// `starts[i]` is the byte offset of line `i`; a final sentinel equals `bytes.len()`.
    /// Files larger than 4 GiB are rejected at load so `u32` offsets are exact.
    starts: Vec<u32>,
    pub binary: bool,
}

/// Files at or below this size are read into memory; larger ones are mapped. Reading small
/// files avoids mmap setup cost and page faults on the first scan.
const MMAP_THRESHOLD: u64 = 1 << 20;

impl FileData {
    pub fn load(path: &Path) -> io::Result<FileData> {
        let _s = trace::span("file-load");
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        if len >= u32::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "files larger than 4 GiB are not supported",
            ));
        }
        let bytes = if len > MMAP_THRESHOLD {
            // Safety: the file is opened read-only; concurrent modification would be a user
            // error and at worst shows garbage, never UB in our own reads of plain bytes.
            let map = unsafe { memmap2::Mmap::map(&file)? };
            #[cfg(unix)]
            let _ = map.advise(memmap2::Advice::Sequential);
            Bytes::Mapped(map)
        } else {
            let mut v = Vec::with_capacity(len as usize);
            io::Read::read_to_end(&mut &file, &mut v)?;
            Bytes::Owned(v)
        };
        Ok(Self::from_bytes(bytes))
    }

    pub fn from_bytes(bytes: Bytes) -> FileData {
        let _s = trace::span_arg("line-index", bytes.len() as u64);
        let binary = bytes[..bytes.len().min(8192)].contains(&0);
        let mut starts = Vec::with_capacity(bytes.len() / 32 + 2);
        starts.push(0);
        for nl in memchr::memchr_iter(b'\n', &bytes) {
            starts.push((nl + 1) as u32);
        }
        // A trailing newline does not start a new (empty) line; for an empty file this also
        // drops the initial 0 so the sentinel is the only entry.
        if *starts.last().unwrap() as usize == bytes.len() {
            starts.pop();
        }
        starts.push(bytes.len() as u32);
        FileData {
            bytes,
            starts,
            binary,
        }
    }

    pub fn empty() -> FileData {
        Self::from_bytes(Bytes::Owned(Vec::new()))
    }

    pub fn line_count(&self) -> usize {
        self.starts.len() - 1
    }

    /// Line content without the trailing `\n`. A trailing `\r` is kept: it is real content
    /// that the diff must see, and the renderer draws it as a visible marker.
    pub fn line(&self, i: usize) -> &[u8] {
        let start = self.starts[i] as usize;
        let mut end = self.starts[i + 1] as usize;
        if end > start && self.bytes[end - 1] == b'\n' {
            end -= 1;
        }
        &self.bytes[start..end]
    }

    pub fn lines(&self) -> impl Iterator<Item = &[u8]> + '_ {
        (0..self.line_count()).map(move |i| self.line(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(s: &str) -> FileData {
        FileData::from_bytes(Bytes::Owned(s.as_bytes().to_vec()))
    }

    #[test]
    fn line_splitting() {
        let f = fd("a\nb\n");
        assert_eq!(f.line_count(), 2);
        assert_eq!(f.line(0), b"a");
        assert_eq!(f.line(1), b"b");

        let f = fd("a\nb");
        assert_eq!(f.line_count(), 2);
        assert_eq!(f.line(1), b"b");

        let f = fd("");
        assert_eq!(f.line_count(), 0);

        let f = fd("\n");
        assert_eq!(f.line_count(), 1);
        assert_eq!(f.line(0), b"");

        let f = fd("x\r\ny");
        assert_eq!(f.line(0), b"x\r");
    }
}
