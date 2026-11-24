//! Multi-HLOD `.cadpack` streaming container and paginated virtual assembly tree index.

#![forbid(unsafe_code)]

pub mod digest;
pub mod format;
pub mod hlod;
pub mod tree;

pub use digest::{RoiInterfaceDigest, build_roi_interface_digest};
pub use format::{CadpackReader, CadpackWriter, ChunkId, ChunkKind};
pub use hlod::{ExactPickRecord, ExactPickTable, HlodLevel, PrototypeHlodChunk};
pub use tree::{AssemblyTreeNode, TreePageSlice, VirtualAssemblyIndex};
