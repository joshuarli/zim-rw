mod codec;
mod format;
mod reader;
mod writer;

pub use codec::compress;
pub use format::{MAGIC, NAMESPACE_CONTENT, NAMESPACE_LISTING, NAMESPACE_METADATA, NAMESPACE_WELL_KNOWN};
pub use reader::{Blob, Entry, Reader};
pub use writer::Writer;

use std::fmt;
use std::io;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned while reading or writing ZIM archives.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u16),
    ShortHeader(usize),
    InconsistentHeaderOffsets,
    NoMainPage,
    NotFound { namespace: u8, url: String },
    RedirectLoop,
    MissingRedirectTarget { redirect: String, target: String },
    UnterminatedUrl(u64),
    UnterminatedTitle(u64),
    BadClusterBounds(u32),
    BlobOutOfRange { cluster: u32, blob: u32 },
    BadBlobOffsets(u32),
    UnsupportedCompression(u8),
    UnsupportedXz,
    InvalidReadLength,
    SizeOverflow,
    Compression(String),
}

impl Error {
    /// Returns true when this error represents a missing entry lookup.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::BadMagic => write!(f, "zim: bad magic, not a ZIM file"),
            Self::UnsupportedVersion(version) => {
                write!(f, "zim: unsupported major version {version}")
            }
            Self::ShortHeader(n) => write!(f, "zim: short header: {n} bytes"),
            Self::InconsistentHeaderOffsets => write!(f, "zim: inconsistent header offsets"),
            Self::NoMainPage => write!(f, "zim: no main page"),
            Self::NotFound { namespace, url } => {
                write!(f, "zim: not found: {}/{}", *namespace as char, url)
            }
            Self::RedirectLoop => write!(f, "zim: redirect loop"),
            Self::MissingRedirectTarget { redirect, target } => {
                write!(
                    f,
                    "zim: redirect {redirect:?} points at missing target {target:?}"
                )
            }
            Self::UnterminatedUrl(off) => write!(f, "zim: unterminated url at {off}"),
            Self::UnterminatedTitle(off) => write!(f, "zim: unterminated title at {off}"),
            Self::BadClusterBounds(cluster) => {
                write!(f, "zim: bad cluster bounds for {cluster}")
            }
            Self::BlobOutOfRange { cluster, blob } => {
                write!(f, "zim: blob {blob} out of range in cluster {cluster}")
            }
            Self::BadBlobOffsets(cluster) => {
                write!(f, "zim: bad blob offsets in cluster {cluster}")
            }
            Self::UnsupportedCompression(comp) => {
                write!(f, "zim: unknown compression {comp}")
            }
            Self::UnsupportedXz => write!(f, "zim: xz clusters are not supported for reading"),
            Self::InvalidReadLength => write!(f, "zim: negative read length"),
            Self::SizeOverflow => write!(f, "zim: archive size is too large for this platform"),
            Self::Compression(err) => write!(f, "zim: compression error: {err}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
