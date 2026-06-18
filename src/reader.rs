use crate::codec::zstd_decode;
use crate::format::{
    le_u16, le_u32, le_u64, parse_header, COMP_NONE, COMP_XZ, COMP_ZSTD, EXTENDED_FLAG, HEADER_LEN,
    NO_MAIN_PAGE, REDIRECT_ENTRY,
};
use crate::{Error, Result};
use memchr::memchr;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The result of a lookup: resolved entry bytes and metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Blob {
    pub namespace: u8,
    pub url: String,
    pub title: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// One directory entry as stored in the archive.
///
/// Redirect entries keep `data` empty and name their target in
/// `redirect_namespace` and `redirect_url`. Unlike [`Reader::get`],
/// [`Reader::entry_at`] does not follow redirects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub namespace: u8,
    pub url: String,
    pub title: String,
    pub mime_type: String,
    pub redirect: bool,
    pub redirect_namespace: u8,
    pub redirect_url: String,
    pub data: Vec<u8>,
}

/// Random-access reader for ZIM archives.
pub struct Reader<R> {
    inner: R,
    size: u64,
    hdr: crate::format::Header,
    mimes: Vec<String>,
    cache: HashMap<u32, CacheEntry>,
}

struct CacheEntry {
    data: Vec<u8>,
    extended: bool,
}

#[derive(Debug)]
struct Dirent {
    mime_idx: u16,
    namespace: u8,
    url: String,
    title: String,
    cluster: u32,
    blob: u32,
    redirect: bool,
    target_index: u32,
}

impl Reader<File> {
    /// Opens a ZIM file on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Self::new(file, size)
    }
}

impl<R: Read + Seek> Reader<R> {
    /// Reads the header and MIME list from `inner`, which must hold `size` bytes.
    pub fn new(inner: R, size: u64) -> Result<Self> {
        let mut r = Self {
            inner,
            size,
            hdr: crate::format::Header::default(),
            mimes: Vec::new(),
            cache: HashMap::new(),
        };
        let hb = r.at(0, HEADER_LEN)?;
        r.hdr = parse_header(&hb)?;
        if r.hdr.mime_list_pos > r.hdr.url_ptr_pos || r.hdr.url_ptr_pos > size {
            return Err(Error::InconsistentHeaderOffsets);
        }
        let mime_len = usize::try_from(r.hdr.url_ptr_pos - r.hdr.mime_list_pos)
            .map_err(|_| Error::SizeOverflow)?;
        let mb = r.at(r.hdr.mime_list_pos, mime_len)?;
        let mut start = 0;
        while let Some(end) = memchr(0, &mb[start..]).map(|idx| start + idx) {
            if end == start {
                break;
            }
            r.mimes
                .push(String::from_utf8_lossy(&mb[start..end]).into_owned());
            start = end + 1;
        }
        Ok(r)
    }

    /// Returns the number of directory entries.
    pub fn count(&self) -> u32 {
        self.hdr.article_count
    }

    /// Returns the archive's MIME-type list.
    pub fn mime_types(&self) -> &[String] {
        &self.mimes
    }

    /// Returns the archive's entry point, or an error if none is set.
    pub fn main_page(&mut self) -> Result<Blob> {
        if self.hdr.main_page == NO_MAIN_PAGE {
            return Err(Error::NoMainPage);
        }
        self.blob_at_index(self.hdr.main_page, 0)
    }

    /// Verifies the trailing MD5 checksum against the archive body.
    pub fn check(&mut self) -> Result<bool> {
        if self.hdr.checksum_pos + 16 > self.size {
            return Ok(false);
        }
        let mut md5 = md5::Context::new();
        let mut remaining = self.hdr.checksum_pos;
        let mut offset = 0;
        while remaining > 0 {
            let n = remaining.min(64 * 1024) as usize;
            let chunk = self.at(offset, n)?;
            md5.consume(&chunk);
            offset += n as u64;
            remaining -= n as u64;
        }
        let expected = self.at(self.hdr.checksum_pos, 16)?;
        Ok(md5.finalize().0 == expected.as_slice())
    }

    /// Returns one stored directory entry in URL order.
    pub fn entry_at(&mut self, idx: u32) -> Result<Entry> {
        let d = self.dirent_at_index(idx)?;
        let mut e = Entry {
            namespace: d.namespace,
            url: d.url,
            title: d.title,
            mime_type: String::new(),
            redirect: d.redirect,
            redirect_namespace: 0,
            redirect_url: String::new(),
            data: Vec::new(),
        };
        if d.redirect {
            let td = self.dirent_at_index(d.target_index)?;
            e.redirect_namespace = td.namespace;
            e.redirect_url = td.url;
            return Ok(e);
        }
        e.data = self.blob_data(d.cluster, d.blob)?;
        if let Some(mime) = self.mimes.get(d.mime_idx as usize) {
            e.mime_type = mime.clone();
        }
        Ok(e)
    }

    /// Returns the namespace and URL of the archive's entry point.
    pub fn main_page_ref(&mut self) -> Result<Option<(u8, String)>> {
        if self.hdr.main_page == NO_MAIN_PAGE {
            return Ok(None);
        }
        let d = self.dirent_at_index(self.hdr.main_page)?;
        Ok(Some((d.namespace, d.url)))
    }

    /// Resolves the entry at `(namespace, url)`, following redirects.
    pub fn get(&mut self, namespace: u8, url: &str) -> Result<Blob> {
        let target = key(namespace, url);
        let mut lo = 0;
        let mut hi = self.hdr.article_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let d = self.dirent_at_index(mid)?;
            match key(d.namespace, &d.url).cmp(&target) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return self.blob_at_index(mid, 0),
            }
        }
        Err(Error::NotFound {
            namespace,
            url: url.to_owned(),
        })
    }

    fn blob_at_index(&mut self, idx: u32, hop: u8) -> Result<Blob> {
        const MAX_REDIRECT_HOPS: u8 = 16;

        if hop > MAX_REDIRECT_HOPS {
            return Err(Error::RedirectLoop);
        }
        let d = self.dirent_at_index(idx)?;
        if d.redirect {
            return self.blob_at_index(d.target_index, hop + 1);
        }
        let data = self.blob_data(d.cluster, d.blob)?;
        let mime_type = self
            .mimes
            .get(d.mime_idx as usize)
            .cloned()
            .unwrap_or_default();
        Ok(Blob {
            namespace: d.namespace,
            url: d.url,
            title: d.title,
            mime_type,
            data,
        })
    }

    fn dirent_at_index(&mut self, idx: u32) -> Result<Dirent> {
        let pb = self.at(self.hdr.url_ptr_pos + 8 * u64::from(idx), 8)?;
        self.dirent_at(le_u64(&pb))
    }

    fn dirent_at(&mut self, off: u64) -> Result<Dirent> {
        let mut window = 512;
        loop {
            let b = self.at_clamped(off, window)?;
            if b.len() < 16 {
                return Err(Error::UnterminatedUrl(off));
            }
            let mut d = Dirent {
                mime_idx: le_u16(&b[0..]),
                namespace: b[3],
                url: String::new(),
                title: String::new(),
                cluster: 0,
                blob: 0,
                redirect: false,
                target_index: 0,
            };
            let p;
            if d.mime_idx == REDIRECT_ENTRY {
                d.redirect = true;
                d.target_index = le_u32(&b[8..]);
                p = 12;
            } else {
                d.cluster = le_u32(&b[8..]);
                d.blob = le_u32(&b[12..]);
                p = 16;
            }
            let Some((url, n1)) = read_c_string(&b, p) else {
                if window >= 1 << 20 || off + window as u64 >= self.size {
                    return Err(Error::UnterminatedUrl(off));
                }
                window *= 4;
                continue;
            };
            let Some((title, _)) = read_c_string(&b, n1) else {
                if window >= 1 << 20 || off + window as u64 >= self.size {
                    return Err(Error::UnterminatedTitle(off));
                }
                window *= 4;
                continue;
            };
            d.url = url;
            d.title = title;
            return Ok(d);
        }
    }

    fn blob_data(&mut self, cluster: u32, blob: u32) -> Result<Vec<u8>> {
        let cached = self.cluster_data(cluster)?;
        let data = &cached.data;
        let extended = cached.extended;
        let width = if extended { 8 } else { 4 };
        let need = usize::try_from((u64::from(blob) + 2) * width as u64)
            .map_err(|_| Error::SizeOverflow)?;
        if need > data.len() {
            return Err(Error::BlobOutOfRange { cluster, blob });
        }
        let o0 = read_uint(&data[(blob as usize * width)..], width);
        let o1 = read_uint(&data[((blob as usize + 1) * width)..], width);
        let o0_usize = usize::try_from(o0).map_err(|_| Error::SizeOverflow)?;
        let o1_usize = usize::try_from(o1).map_err(|_| Error::SizeOverflow)?;
        if o0 > o1 || o1_usize > data.len() {
            return Err(Error::BadBlobOffsets(cluster));
        }
        Ok(data[o0_usize..o1_usize].to_vec())
    }

    fn cluster_data(&mut self, cluster: u32) -> Result<&CacheEntry> {
        if self.cache.contains_key(&cluster) {
            return Ok(self.cache.get(&cluster).expect("cache entry exists"));
        }

        let start = self.cluster_offset(cluster)?;
        let end = if cluster + 1 < self.hdr.cluster_count {
            self.cluster_offset(cluster + 1)?
        } else {
            self.hdr.checksum_pos
        };
        if start >= end || end > self.size {
            return Err(Error::BadClusterBounds(cluster));
        }
        let raw_len = usize::try_from(end - start).map_err(|_| Error::SizeOverflow)?;
        let raw = self.at(start, raw_len)?;
        let info = raw[0];
        let comp = info & 0x0f;
        let extended = info & EXTENDED_FLAG != 0;
        let body = &raw[1..];
        let data = match comp {
            0 | COMP_NONE => body.to_vec(),
            COMP_ZSTD => zstd_decode(body)?,
            COMP_XZ => return Err(Error::UnsupportedXz),
            comp => return Err(Error::UnsupportedCompression(comp)),
        };
        self.cache.insert(
            cluster,
            CacheEntry {
                data: data.clone(),
                extended,
            },
        );
        Ok(self.cache.get(&cluster).expect("cache entry was inserted"))
    }

    fn cluster_offset(&mut self, cluster: u32) -> Result<u64> {
        let b = self.at(self.hdr.cluster_ptr_pos + 8 * u64::from(cluster), 8)?;
        Ok(le_u64(&b))
    }

    fn at(&mut self, off: u64, n: usize) -> Result<Vec<u8>> {
        if off > self.size || off.saturating_add(n as u64) > self.size {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let mut b = vec![0; n];
        if n == 0 {
            return Ok(b);
        }
        self.inner.seek(SeekFrom::Start(off))?;
        self.inner.read_exact(&mut b)?;
        Ok(b)
    }

    fn at_clamped(&mut self, off: u64, n: usize) -> Result<Vec<u8>> {
        if off > self.size {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let end = off.saturating_add(n as u64).min(self.size);
        let n = usize::try_from(end - off).map_err(|_| Error::SizeOverflow)?;
        let mut b = vec![0; n];
        if n == 0 {
            return Ok(b);
        }
        self.inner.seek(SeekFrom::Start(off))?;
        self.inner.read_exact(&mut b)?;
        Ok(b)
    }
}

fn key(ns: u8, url: &str) -> String {
    let mut key = String::with_capacity(1 + url.len());
    key.push(ns as char);
    key.push_str(url);
    key
}

fn read_c_string(b: &[u8], start: usize) -> Option<(String, usize)> {
    if start > b.len() {
        return None;
    }
    let i = memchr(0, &b[start..])?;
    Some((
        String::from_utf8_lossy(&b[start..start + i]).into_owned(),
        start + i + 1,
    ))
}

fn read_uint(b: &[u8], width: usize) -> u64 {
    if width == 8 {
        le_u64(b)
    } else {
        u64::from(le_u32(b))
    }
}

pub(crate) fn make_key(ns: u8, url: &str) -> String {
    key(ns, url)
}
