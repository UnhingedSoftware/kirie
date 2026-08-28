pub const MAGIC: &[u8; 8] = b"KIRIESFX";

pub const KEY_LEN: usize = 16;

pub const TRAILER_LEN: usize = 8 + 8 + KEY_LEN;
