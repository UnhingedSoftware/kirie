use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("reading {path}: {source}")]
    File {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("not a kirie package (the file does not start with KIRIEPKG)")]
    NotAPackage,

    #[error("this package is format version {0}; this kirie reads version 1 only")]
    UnsupportedVersion(u16),

    #[error(
        "this package uses features this kirie does not have (flags {0:#06x}); it may be encrypted or signed"
    )]
    UnsupportedFlags(u16),

    #[error("the package is damaged: {0}")]
    Corrupt(String),

    #[error("the manifest is not valid: {0}")]
    BadManifest(String),

    #[error("entry path {path:?} is not allowed: {why}")]
    BadPath { path: String, why: &'static str },

    #[error("the package has two entries named {0:?}")]
    DuplicatePath(String),

    #[error("the package has no entry named {0:?}")]
    NoSuchEntry(String),

    #[error("entry {path:?} does not match its hash; the file was damaged or changed")]
    HashMismatch { path: String },

    #[error("entry {path:?} is stored with {what}, which this kirie cannot read")]
    UnsupportedStorage { path: String, what: String },
}

impl PackError {
    pub(crate) fn corrupt(why: impl Into<String>) -> Self {
        PackError::Corrupt(why.into())
    }
}
