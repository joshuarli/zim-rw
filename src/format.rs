use crate::{Error, Result};

pub const MAGIC: u32 = 0x44D495A;

const MAJOR_VERSION: u16 = 5;
const MINOR_VERSION: u16 = 1;
pub(crate) const HEADER_LEN: usize = 80;

pub const NAMESPACE_CONTENT: u8 = b'C';
pub const NAMESPACE_METADATA: u8 = b'M';
pub const NAMESPACE_WELL_KNOWN: u8 = b'W';

pub(crate) const COMP_NONE: u8 = 1;
pub(crate) const COMP_XZ: u8 = 4;
pub(crate) const COMP_ZSTD: u8 = 5;
pub(crate) const EXTENDED_FLAG: u8 = 0x10;

pub(crate) const REDIRECT_ENTRY: u16 = 0xffff;
pub(crate) const NO_MAIN_PAGE: u32 = 0xffffffff;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Header {
    pub uuid: [u8; 16],
    pub article_count: u32,
    pub cluster_count: u32,
    pub url_ptr_pos: u64,
    pub title_ptr_pos: u64,
    pub cluster_ptr_pos: u64,
    pub mime_list_pos: u64,
    pub main_page: u32,
    pub layout_page: u32,
    pub checksum_pos: u64,
}

impl Header {
    pub fn marshal(self) -> [u8; HEADER_LEN] {
        let mut b = [0; HEADER_LEN];
        put_u32(&mut b[0..], MAGIC);
        put_u16(&mut b[4..], MAJOR_VERSION);
        put_u16(&mut b[6..], MINOR_VERSION);
        b[8..24].copy_from_slice(&self.uuid);
        put_u32(&mut b[24..], self.article_count);
        put_u32(&mut b[28..], self.cluster_count);
        put_u64(&mut b[32..], self.url_ptr_pos);
        put_u64(&mut b[40..], self.title_ptr_pos);
        put_u64(&mut b[48..], self.cluster_ptr_pos);
        put_u64(&mut b[56..], self.mime_list_pos);
        put_u32(&mut b[64..], self.main_page);
        put_u32(&mut b[68..], self.layout_page);
        put_u64(&mut b[72..], self.checksum_pos);
        b
    }
}

pub(crate) fn parse_header(b: &[u8]) -> Result<Header> {
    if b.len() < HEADER_LEN {
        return Err(Error::ShortHeader(b.len()));
    }
    if le_u32(&b[0..]) != MAGIC {
        return Err(Error::BadMagic);
    }
    let major_version = le_u16(&b[4..]);
    if major_version != 5 && major_version != 6 {
        return Err(Error::UnsupportedVersion(major_version));
    }
    let mut h = Header::default();
    h.uuid.copy_from_slice(&b[8..24]);
    h.article_count = le_u32(&b[24..]);
    h.cluster_count = le_u32(&b[28..]);
    h.url_ptr_pos = le_u64(&b[32..]);
    h.title_ptr_pos = le_u64(&b[40..]);
    h.cluster_ptr_pos = le_u64(&b[48..]);
    h.mime_list_pos = le_u64(&b[56..]);
    h.main_page = le_u32(&b[64..]);
    h.layout_page = le_u32(&b[68..]);
    h.checksum_pos = le_u64(&b[72..]);
    Ok(h)
}

pub(crate) fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().expect("slice has enough bytes"))
}

pub(crate) fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().expect("slice has enough bytes"))
}

pub(crate) fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().expect("slice has enough bytes"))
}

pub(crate) fn put_u16(b: &mut [u8], v: u16) {
    b[..2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u32(b: &mut [u8], v: u32) {
    b[..4].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u64(b: &mut [u8], v: u64) {
    b[..8].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn md5(input: &[u8]) -> [u8; 16] {
    md5::compute(input).0
}
