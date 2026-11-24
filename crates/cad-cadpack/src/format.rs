//! The on-disk `.cadpack` v1 container.
//!
//! A pack consists of a fixed 64-byte header, a 64-byte aligned directory and
//! 64-byte aligned chunk payloads.  All integer fields are little endian.  The
//! directory contains the SHA-256 digest of each payload, so a range fetched
//! over HTTP can be verified before it is handed to a decoder.
//!
//! The header fields occupy these byte ranges: `0..4` magic (`CPK1`), `4..8`
//! format version, `8..16` directory offset, `16..20` directory entry count,
//! `20..24` directory entry size, and `24..32` total file size. The remaining
//! header bytes are reserved and must not be used for payload data. Each
//! directory entry uses `0..4` for the chunk ID, `4..8` for the chunk kind,
//! `8..16` for the payload offset, `16..24` for the payload length, and
//! `24..56` for the SHA-256 digest. Bytes `56..64` in an entry are reserved.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// The fixed size of the container header and each directory entry.
pub const ALIGNMENT: u64 = 64;
/// The `.cadpack` format version implemented by this module.
pub const FORMAT_VERSION: u32 = 1;
/// The four-byte container signature.
pub const MAGIC: [u8; 4] = *b"CPK1";
/// The fixed header size in bytes.
pub const HEADER_SIZE: usize = ALIGNMENT as usize;
/// The fixed directory entry size in bytes.
pub const DIRECTORY_ENTRY_SIZE: usize = ALIGNMENT as usize;

/// Identifier assigned by a producer to a chunk.
pub type ChunkId = u32;

/// The semantic payload type of a chunk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub enum ChunkKind {
    Hlod0 = 0,
    Hlod1 = 1,
    Hlod2 = 2,
    ExactPickTable = 3,
    VirtualTree = 4,
    InterfaceDigest = 5,
}

impl ChunkKind {
    fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Hlod0),
            1 => Some(Self::Hlod1),
            2 => Some(Self::Hlod2),
            3 => Some(Self::ExactPickTable),
            4 => Some(Self::VirtualTree),
            5 => Some(Self::InterfaceDigest),
            _ => None,
        }
    }
}

/// A directory record.  `offset` and `length` describe the payload byte range
/// and are suitable for an HTTP Range request after converting the range to an
/// inclusive end (`offset + length - 1` for a non-empty chunk).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkDirectoryEntry {
    pub chunk_id: ChunkId,
    pub kind: ChunkKind,
    pub offset: u64,
    pub length: u64,
    pub sha256: [u8; 32],
}

/// Errors returned while encoding or decoding a `.cadpack`.
#[derive(Debug, Error)]
pub enum CadpackError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid cadpack magic")]
    InvalidMagic,
    #[error("unsupported cadpack format version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid cadpack header: {0}")]
    InvalidHeader(&'static str),
    #[error("invalid cadpack directory: {0}")]
    InvalidDirectory(&'static str),
    #[error("chunk id {0} is already present")]
    DuplicateChunkId(ChunkId),
    #[error("chunk id {0} was not found")]
    ChunkNotFound(ChunkId),
    #[error("chunk {0} is too large for this platform")]
    ChunkTooLarge(ChunkId),
    #[error("SHA-256 verification failed for chunk {chunk_id}")]
    HashMismatch {
        chunk_id: ChunkId,
        expected: [u8; 32],
        actual: [u8; 32],
    },
}

/// A complete pack assembled in memory before it is written.
#[derive(Debug, Default)]
pub struct CadpackWriter {
    chunks: Vec<PendingChunk>,
    chunk_ids: HashSet<ChunkId>,
}

#[derive(Debug)]
struct PendingChunk {
    chunk_id: ChunkId,
    kind: ChunkKind,
    bytes: Vec<u8>,
}

impl CadpackWriter {
    /// Creates an empty pack writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a chunk. IDs must be unique within a pack.
    pub fn add_chunk(
        &mut self,
        chunk_id: ChunkId,
        kind: ChunkKind,
        bytes: Vec<u8>,
    ) -> Result<(), CadpackError> {
        if !self.chunk_ids.insert(chunk_id) {
            return Err(CadpackError::DuplicateChunkId(chunk_id));
        }
        self.chunks.push(PendingChunk {
            chunk_id,
            kind,
            bytes,
        });
        Ok(())
    }

    /// Returns the number of chunks queued for writing.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Writes the complete container to any sequential writer.
    pub fn write_to<W: Write>(&self, mut output: W) -> Result<(), CadpackError> {
        let count = u32::try_from(self.chunks.len())
            .map_err(|_| CadpackError::InvalidHeader("too many chunks"))?;
        let directory_bytes = u64::from(count)
            .checked_mul(ALIGNMENT)
            .ok_or(CadpackError::InvalidHeader("directory size overflow"))?;
        let directory_offset = ALIGNMENT;
        let payload_start = align_up(
            directory_offset
                .checked_add(directory_bytes)
                .ok_or(CadpackError::InvalidHeader("directory end overflow"))?,
        )
        .ok_or(CadpackError::InvalidHeader("payload start overflow"))?;

        let mut entries = Vec::with_capacity(self.chunks.len());
        let mut next_offset = payload_start;
        for chunk in &self.chunks {
            let offset = align_up(next_offset)
                .ok_or(CadpackError::InvalidHeader("chunk offset overflow"))?;
            let length = u64::try_from(chunk.bytes.len())
                .map_err(|_| CadpackError::InvalidHeader("chunk length overflow"))?;
            let end = offset
                .checked_add(length)
                .ok_or(CadpackError::InvalidHeader("chunk end overflow"))?;
            let digest = Sha256::digest(&chunk.bytes);
            let mut sha256 = [0_u8; 32];
            sha256.copy_from_slice(&digest);
            entries.push(ChunkDirectoryEntry {
                chunk_id: chunk.chunk_id,
                kind: chunk.kind,
                offset,
                length,
                sha256,
            });
            next_offset = end;
        }
        let total_size =
            align_up(next_offset).ok_or(CadpackError::InvalidHeader("total size overflow"))?;

        let mut header = [0_u8; HEADER_SIZE];
        header[0..4].copy_from_slice(&MAGIC);
        header[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&directory_offset.to_le_bytes());
        header[16..20].copy_from_slice(&count.to_le_bytes());
        header[20..24].copy_from_slice(&(DIRECTORY_ENTRY_SIZE as u32).to_le_bytes());
        header[24..32].copy_from_slice(&total_size.to_le_bytes());
        output.write_all(&header)?;

        for entry in &entries {
            write_directory_entry(&mut output, entry)?;
        }

        let mut written = directory_offset
            .checked_add(directory_bytes)
            .ok_or(CadpackError::InvalidHeader("directory end overflow"))?;
        for (chunk, entry) in self.chunks.iter().zip(&entries) {
            if entry.offset > written {
                write_zeroes(&mut output, entry.offset - written)?;
            }
            output.write_all(&chunk.bytes)?;
            written = entry
                .offset
                .checked_add(entry.length)
                .ok_or(CadpackError::InvalidHeader("chunk end overflow"))?;
        }
        if total_size > written {
            write_zeroes(&mut output, total_size - written)?;
        }
        Ok(())
    }
}

/// A reader that keeps the source seekable and only loads a requested chunk.
#[derive(Debug)]
pub struct CadpackReader<R> {
    source: R,
    entries: Vec<ChunkDirectoryEntry>,
    by_id: HashMap<ChunkId, usize>,
    total_size: u64,
}

impl<R: Read + Seek> CadpackReader<R> {
    /// Parses and validates a complete `.cadpack` source.
    pub fn new(mut source: R) -> Result<Self, CadpackError> {
        let file_size = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;
        let mut header = [0_u8; HEADER_SIZE];
        source.read_exact(&mut header)?;
        if header[0..4] != MAGIC {
            return Err(CadpackError::InvalidMagic);
        }
        let version = read_u32(&header, 4);
        if version != FORMAT_VERSION {
            return Err(CadpackError::UnsupportedVersion(version));
        }
        let directory_offset = read_u64(&header, 8);
        let count = read_u32(&header, 16);
        let entry_size = read_u32(&header, 20);
        let total_size = read_u64(&header, 24);
        if !directory_offset.is_multiple_of(ALIGNMENT) {
            return Err(CadpackError::InvalidHeader(
                "directory is not 64-byte aligned",
            ));
        }
        if directory_offset < HEADER_SIZE as u64 {
            return Err(CadpackError::InvalidHeader("directory overlaps header"));
        }
        if entry_size as usize != DIRECTORY_ENTRY_SIZE {
            return Err(CadpackError::InvalidHeader(
                "unsupported directory entry size",
            ));
        }
        if total_size != file_size {
            return Err(CadpackError::InvalidHeader(
                "total size does not match source",
            ));
        }
        let directory_bytes = u64::from(count)
            .checked_mul(ALIGNMENT)
            .ok_or(CadpackError::InvalidDirectory("directory size overflow"))?;
        let directory_end = directory_offset
            .checked_add(directory_bytes)
            .ok_or(CadpackError::InvalidDirectory("directory end overflow"))?;
        if directory_end > file_size {
            return Err(CadpackError::InvalidDirectory(
                "directory is outside source",
            ));
        }

        source.seek(SeekFrom::Start(directory_offset))?;
        // Do not reserve based on an untrusted count. The directory bytes were
        // bounds-checked above, but a valid sparse file can still advertise a
        // very large count. Vec and HashMap grow incrementally from this cap.
        let initial_capacity = usize::try_from(count).unwrap_or(usize::MAX).min(1024);
        let mut entries = Vec::with_capacity(initial_capacity);
        let mut by_id = HashMap::with_capacity(initial_capacity);
        let mut raw_entry = [0_u8; DIRECTORY_ENTRY_SIZE];
        for _ in 0..count {
            source.read_exact(&mut raw_entry)?;
            let chunk_id = read_u32(&raw_entry, 0);
            let kind_raw = read_u32(&raw_entry, 4);
            let kind = ChunkKind::from_raw(kind_raw)
                .ok_or(CadpackError::InvalidDirectory("unknown chunk kind"))?;
            let offset = read_u64(&raw_entry, 8);
            let length = read_u64(&raw_entry, 16);
            let mut sha256 = [0_u8; 32];
            sha256.copy_from_slice(&raw_entry[24..56]);
            if by_id.insert(chunk_id, entries.len()).is_some() {
                return Err(CadpackError::InvalidDirectory("duplicate chunk id"));
            }
            if !offset.is_multiple_of(ALIGNMENT) {
                return Err(CadpackError::InvalidDirectory(
                    "chunk is not 64-byte aligned",
                ));
            }
            let end = offset
                .checked_add(length)
                .ok_or(CadpackError::InvalidDirectory("chunk range overflow"))?;
            if offset < directory_end || end > file_size {
                return Err(CadpackError::InvalidDirectory(
                    "chunk range is outside source",
                ));
            }
            entries.push(ChunkDirectoryEntry {
                chunk_id,
                kind,
                offset,
                length,
                sha256,
            });
        }
        let mut ranges = entries
            .iter()
            .filter(|entry| entry.length != 0)
            .map(|entry| {
                let end = entry
                    .offset
                    .checked_add(entry.length)
                    .ok_or(CadpackError::InvalidDirectory("chunk range overflow"))?;
                Ok((entry.offset, end))
            })
            .collect::<Result<Vec<_>, CadpackError>>()?;
        ranges.sort_unstable_by_key(|range| range.0);
        for pair in ranges.windows(2) {
            if pair[1].0 < pair[0].1 {
                return Err(CadpackError::InvalidDirectory("overlapping chunk ranges"));
            }
        }

        Ok(Self {
            source,
            entries,
            by_id,
            total_size,
        })
    }

    /// Returns the validated directory entries in on-disk order.
    pub fn directory(&self) -> &[ChunkDirectoryEntry] {
        &self.entries
    }

    /// Returns the total byte length of the source container.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Returns `(offset, length)` for the payload, suitable for HTTP Range.
    pub fn chunk_range(&self, chunk_id: ChunkId) -> Result<(u64, u64), CadpackError> {
        let entry = self.entry(chunk_id)?;
        Ok((entry.offset, entry.length))
    }

    /// Reads and SHA-256 verifies one payload in O(1) directory lookup time.
    pub fn read_chunk_bytes(&mut self, chunk_id: ChunkId) -> Result<Vec<u8>, CadpackError> {
        let entry = *self.entry(chunk_id)?;
        if entry.length > isize::MAX as u64 {
            return Err(CadpackError::ChunkTooLarge(chunk_id));
        }
        let length =
            usize::try_from(entry.length).map_err(|_| CadpackError::ChunkTooLarge(chunk_id))?;
        self.source.seek(SeekFrom::Start(entry.offset))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| CadpackError::ChunkTooLarge(chunk_id))?;
        bytes.resize(length, 0);
        self.source.read_exact(&mut bytes)?;
        self.verify_bytes(entry, &bytes)?;
        Ok(bytes)
    }

    /// Verifies bytes fetched by an external range transport against the
    /// directory digest without reading from the source.
    pub fn verify_chunk_bytes(&self, chunk_id: ChunkId, bytes: &[u8]) -> Result<(), CadpackError> {
        let entry = *self.entry(chunk_id)?;
        if bytes.len() as u64 != entry.length {
            return Err(CadpackError::InvalidDirectory(
                "range length does not match chunk",
            ));
        }
        self.verify_bytes(entry, bytes)
    }

    fn entry(&self, chunk_id: ChunkId) -> Result<&ChunkDirectoryEntry, CadpackError> {
        self.by_id
            .get(&chunk_id)
            .map(|index| &self.entries[*index])
            .ok_or(CadpackError::ChunkNotFound(chunk_id))
    }

    fn verify_bytes(&self, entry: ChunkDirectoryEntry, bytes: &[u8]) -> Result<(), CadpackError> {
        let digest = Sha256::digest(bytes);
        let mut actual = [0_u8; 32];
        actual.copy_from_slice(&digest);
        if actual != entry.sha256 {
            return Err(CadpackError::HashMismatch {
                chunk_id: entry.chunk_id,
                expected: entry.sha256,
                actual,
            });
        }
        Ok(())
    }
}

impl fmt::Display for ChunkKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Hlod0 => "Hlod0",
            Self::Hlod1 => "Hlod1",
            Self::Hlod2 => "Hlod2",
            Self::ExactPickTable => "ExactPickTable",
            Self::VirtualTree => "VirtualTree",
            Self::InterfaceDigest => "InterfaceDigest",
        })
    }
}

fn align_up(value: u64) -> Option<u64> {
    value
        .checked_add(ALIGNMENT - 1)
        .map(|aligned| aligned / ALIGNMENT * ALIGNMENT)
}

fn write_directory_entry<W: Write>(
    output: &mut W,
    entry: &ChunkDirectoryEntry,
) -> Result<(), CadpackError> {
    let mut raw = [0_u8; DIRECTORY_ENTRY_SIZE];
    raw[0..4].copy_from_slice(&entry.chunk_id.to_le_bytes());
    raw[4..8].copy_from_slice(&(entry.kind as u32).to_le_bytes());
    raw[8..16].copy_from_slice(&entry.offset.to_le_bytes());
    raw[16..24].copy_from_slice(&entry.length.to_le_bytes());
    raw[24..56].copy_from_slice(&entry.sha256);
    output.write_all(&raw)?;
    Ok(())
}

fn write_zeroes<W: Write>(output: &mut W, mut length: u64) -> Result<(), io::Error> {
    const ZEROES: [u8; 4096] = [0; 4096];
    while length != 0 {
        let count = length.min(ZEROES.len() as u64) as usize;
        output.write_all(&ZEROES[..count])?;
        length -= count as u64;
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0_u8; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0_u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}
