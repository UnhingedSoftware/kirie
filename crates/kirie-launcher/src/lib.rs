//! Shared trailer format for the kirie self-extracting single-file binary.
//!
//! The single file is laid out as:
//!
//! ```text
//! [ launcher stub ELF ][ zstd(tar(runtime dir)) ][ TRAILER ]
//! ```
//!
//! where the fixed-size trailer at the very end is:
//!
//! ```text
//! MAGIC (8) | blob_len: u64 LE (8) | cache_key: ascii (16)
//! ```
//!
//! The launcher reads the last [`TRAILER_LEN`] bytes of its own executable,
//! validates [`MAGIC`], and uses `cache_key` to find (or `blob_len` to extract)
//! the runtime under the cache dir. `cache_key` is the first 16 hex chars of the
//! blake3 hash of the compressed blob, so a byte-identical build reuses the same
//! extracted runtime and a changed build extracts fresh.

/// Marks a kirie self-extracting binary; the last 8 bytes before the length.
pub const MAGIC: &[u8; 8] = b"KIRIESFX";

/// Length of the `cache_key` field (hex chars of a truncated blake3 hash).
pub const KEY_LEN: usize = 16;

/// Total trailer length: [`MAGIC`] (8) + `blob_len` u64 (8) + key ([`KEY_LEN`]).
pub const TRAILER_LEN: usize = 8 + 8 + KEY_LEN;
