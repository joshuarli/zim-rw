use crate::codec::zstd_decode;
use crate::format::{
    COMP_NONE, COMP_XZ, COMP_ZSTD, EXTENDED_FLAG, HEADER_LEN, NO_MAIN_PAGE, REDIRECT_ENTRY, le_u16,
    le_u32, le_u64, parse_header,
};
use crate::{Error, Result};
use memchr::memchr;
use memmap2::Mmap;
use rustc_hash::FxHashMap;
use std::fs::File;
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

enum Backing {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Backing {
    fn as_slice(&self) -> &[u8] {
        match self {
            Backing::Mapped(m) => m.as_ref(),
            Backing::Owned(v) => v.as_slice(),
        }
    }
}

/// Random-access reader for ZIM archives.
pub struct Reader {
    data: Backing,
    size: u64,
    hdr: crate::format::Header,
    mimes: Vec<String>,
    cache: FxHashMap<u32, CacheEntry>,
    max_cache: usize,
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

impl Reader {
    /// Opens a ZIM file on disk, memory-mapping it for zero-copy access.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        // Safety: the file is opened read-only; Mmap::map is the safe wrapper
        // around the platform mmap syscall provided by memmap2.
        let mmap = unsafe { Mmap::map(&file)? };
        Self::from_backing(Backing::Mapped(mmap), size)
    }

    /// Creates a reader from an in-memory byte buffer.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        let size = data.len() as u64;
        Self::from_backing(Backing::Owned(data), size)
    }

    fn from_backing(data: Backing, size: u64) -> Result<Self> {
        // Access the backing directly during construction to avoid borrow
        // conflicts between at()-returned slices and field mutation.
        let backing_slice = data.as_slice();

        let hb = backing_slice
            .get(..HEADER_LEN)
            .ok_or(Error::ShortHeader(backing_slice.len()))?;
        let hdr = parse_header(hb)?;
        if hdr.mime_list_pos > hdr.url_ptr_pos || hdr.url_ptr_pos > size {
            return Err(Error::InconsistentHeaderOffsets);
        }
        let mime_len = usize::try_from(hdr.url_ptr_pos - hdr.mime_list_pos)
            .map_err(|_| Error::SizeOverflow)?;
        let mime_start = hdr.mime_list_pos as usize;
        let mime_end = mime_start
            .checked_add(mime_len)
            .ok_or(Error::SizeOverflow)?;
        let mb = backing_slice
            .get(mime_start..mime_end)
            .ok_or_else(|| Error::Io(std::io::ErrorKind::UnexpectedEof.into()))?;

        let mut mimes = Vec::new();
        let mut start = 0;
        while let Some(end) = memchr(0, &mb[start..]).map(|idx| start + idx) {
            if end == start {
                break;
            }
            mimes.push(String::from_utf8_lossy(&mb[start..end]).into_owned());
            start = end + 1;
        }

        Ok(Self {
            data,
            size,
            hdr,
            mimes,
            cache: FxHashMap::default(),
            max_cache: 64,
        })
    }

    /// Returns the number of directory entries.
    pub fn count(&self) -> u32 {
        self.hdr.article_count
    }

    /// Returns the archive's MIME-type list.
    pub fn mime_types(&self) -> &[String] {
        &self.mimes
    }

    /// Sets the maximum number of decompressed clusters kept in memory.
    ///
    /// When the cache reaches this size, it is cleared before inserting the
    /// next entry.  The default is 64.  Pass 0 for unbounded (not recommended
    /// for long-running servers).
    pub fn set_cache_limit(&mut self, limit: usize) {
        self.max_cache = limit;
    }

    /// Returns the current cache limit (0 means unbounded).
    pub fn cache_limit(&self) -> usize {
        self.max_cache
    }

    /// Returns the archive's entry point, or an error if none is set.
    pub fn main_page(&mut self) -> Result<Blob> {
        if self.hdr.main_page == NO_MAIN_PAGE {
            return Err(Error::NoMainPage);
        }
        self.blob_at_index(self.hdr.main_page, 0)
    }

    /// Verifies the trailing MD5 checksum against the archive body.
    pub fn check(&self) -> Result<bool> {
        if self.hdr.checksum_pos + 16 > self.size {
            return Ok(false);
        }
        let mut md5 = md5::Context::new();
        let mut remaining = self.hdr.checksum_pos;
        let mut offset = 0;
        while remaining > 0 {
            let n = remaining.min(64 * 1024) as usize;
            let chunk = self.at(offset, n)?;
            md5.consume(chunk);
            offset += n as u64;
            remaining -= n as u64;
        }
        let expected = self.at(self.hdr.checksum_pos, 16)?;
        Ok(md5.finalize().0 == expected)
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
    pub fn main_page_ref(&self) -> Result<Option<(u8, String)>> {
        if self.hdr.main_page == NO_MAIN_PAGE {
            return Ok(None);
        }
        let d = self.dirent_at_index(self.hdr.main_page)?;
        Ok(Some((d.namespace, d.url)))
    }

    /// Resolves an entry by title using the title pointer table.
    ///
    /// Returns the first entry whose title matches (case-sensitive).
    /// If there are multiple entries with the same title in different
    /// namespaces, `namespace` disambiguates; pass `namespace` from
    /// a prior listing to pick a specific one, or use `0` for any.
    pub fn get_by_title(&mut self, namespace: u8, title: &str) -> Result<Blob> {
        let target = key(namespace, title);
        let count = self.hdr.article_count;
        let title_ptr_size = if count > 0 {
            usize::try_from(
                self.hdr
                    .cluster_ptr_pos
                    .saturating_sub(self.hdr.title_ptr_pos),
            )
            .map_err(|_| Error::SizeOverflow)?
        } else {
            0
        };

        let lo = self.lower_bound_title(&target, title_ptr_size)?;
        if lo >= count {
            return Err(Error::NotFound {
                namespace,
                url: title.to_owned(),
            });
        }

        let d = self.dirent_at_title_index(lo)?;
        let found = if namespace == 0 {
            d.title == title
        } else {
            d.namespace == namespace && d.title == title
        };

        if found {
            let url_idx = self.title_index_to_url_index(lo)?;
            return self.blob_at_index(url_idx, 0);
        }

        Err(Error::NotFound {
            namespace,
            url: title.to_owned(),
        })
    }

    /// Returns all entries whose title starts with `prefix` (case-sensitive).
    pub fn entries_by_title_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>> {
        let target = key(namespace, prefix);
        let count = self.hdr.article_count;
        if count == 0 || prefix.is_empty() {
            return Ok(Vec::new());
        }
        let title_ptr_size = usize::try_from(
            self.hdr
                .cluster_ptr_pos
                .saturating_sub(self.hdr.title_ptr_pos),
        )
        .map_err(|_| Error::SizeOverflow)?;

        let mut results = Vec::new();

        // For namespace == 0 we must scan all entries (title index is sorted
        // by namespace+title, not by title alone, so a binary prefix scan
        // would miss entries in later namespace groups).
        if namespace == 0 {
            for idx in 0..count {
                let d = self.dirent_at_title_index(idx)?;
                if d.title.starts_with(prefix) {
                    let url_idx = self.title_index_to_url_index(idx)?;
                    results.push(self.entry_at(url_idx)?);
                }
            }
            return Ok(results);
        }

        let mut idx = self.lower_bound_title(&target, title_ptr_size)?;
        while idx < count {
            let d = self.dirent_at_title_index(idx)?;
            if d.namespace != namespace || !d.title.starts_with(prefix) {
                if d.namespace > namespace {
                    break;
                }
                if d.namespace == namespace && d.title.as_str() > prefix {
                    break;
                }
                idx += 1;
                continue;
            }
            let url_idx = self.title_index_to_url_index(idx)?;
            results.push(self.entry_at(url_idx)?);
            idx += 1;
        }
        Ok(results)
    }

    /// Reads a byte range `[offset, offset+len)` from a blob.
    ///
    /// For uncompressed clusters the range is sliced directly from the
    /// backing store without decompressing. For compressed clusters the
    /// containing cluster is decompressed and cached as usual, then the
    /// range is extracted.
    pub fn get_range(
        &mut self,
        namespace: u8,
        url: &str,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, u64)> {
        let target = key(namespace, url);
        let mut idx = None;
        {
            let mut lo = 0;
            let mut hi = self.hdr.article_count;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let d = self.dirent_at_index(mid)?;
                match key(d.namespace, &d.url).cmp(&target) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => {
                        idx = Some(mid);
                        break;
                    }
                }
            }
        }
        let mut idx = idx.ok_or_else(|| Error::NotFound {
            namespace,
            url: url.to_owned(),
        })?;

        // Follow redirects
        const MAX_REDIRECT_HOPS: u8 = 16;
        for hop in 0..=MAX_REDIRECT_HOPS {
            let d = self.dirent_at_index(idx)?;
            if !d.redirect {
                return self.blob_data_range(d.cluster, d.blob, offset, len);
            }
            if hop == MAX_REDIRECT_HOPS {
                return Err(Error::RedirectLoop);
            }
            idx = d.target_index;
        }
        unreachable!();
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
}

// Private methods
impl Reader {
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

    /// Like `blob_data` but returns only the requested byte range.
    fn blob_data_range(
        &mut self,
        cluster: u32,
        blob: u32,
        offset: u64,
        len: u64,
    ) -> Result<(Vec<u8>, u64)> {
        let cluster_start = self.cluster_offset(cluster)?;
        let cluster_end = if cluster + 1 < self.hdr.cluster_count {
            self.cluster_offset(cluster + 1)?
        } else {
            self.hdr.checksum_pos
        };
        if cluster_start >= cluster_end || cluster_end > self.size {
            return Err(Error::BadClusterBounds(cluster));
        }

        let backing = self.data.as_slice();
        let info = backing[cluster_start as usize];
        let comp = info & 0x0f;
        let extended = info & EXTENDED_FLAG != 0;
        let width = if extended { 8 } else { 4 };
        let body_start = (cluster_start + 1) as usize;

        // For uncompressed clusters, read the offset table and slice the
        // requested range directly from the backing — no decompression needed.
        if comp == 0 || comp == COMP_NONE {
            let body = &backing[body_start..cluster_end as usize];
            let need = usize::try_from((u64::from(blob) + 2) * width as u64)
                .map_err(|_| Error::SizeOverflow)?;
            if need > body.len() {
                return Err(Error::BlobOutOfRange { cluster, blob });
            }
            let o0 = read_uint(&body[(blob as usize * width)..], width);
            let o1 = read_uint(&body[((blob as usize + 1) * width)..], width);
            let o0_usize = usize::try_from(o0).map_err(|_| Error::SizeOverflow)?;
            let o1_usize = usize::try_from(o1).map_err(|_| Error::SizeOverflow)?;
            if o0 > o1 || o1_usize > body.len() {
                return Err(Error::BadBlobOffsets(cluster));
            }
            let blob_size = o1 - o0;
            let blob_start = o0_usize.saturating_add(offset as usize);
            let blob_end = o1_usize.min(blob_start.saturating_add(len as usize));
            if blob_start >= blob_end || blob_start >= body.len() {
                return Ok((Vec::new(), blob_size));
            }
            let abs_start = body_start + blob_start;
            let abs_end = body_start + blob_end;
            return Ok((backing[abs_start..abs_end].to_vec(), blob_size));
        }

        // For compressed clusters, decompress and cache, then slice.
        let cached = self.cluster_data(cluster)?;
        let data = &cached.data;
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
        let blob_size = o1 - o0;
        let blob_start = o0_usize.saturating_add(offset as usize);
        let blob_end = o1_usize.min(blob_start.saturating_add(len as usize));
        if blob_start >= blob_end || blob_start >= data.len() {
            return Ok((Vec::new(), blob_size));
        }
        Ok((data[blob_start..blob_end].to_vec(), blob_size))
    }

    fn cluster_data(&mut self, cluster: u32) -> Result<&CacheEntry> {
        if self.cache.contains_key(&cluster) {
            return Ok(self.cache.get(&cluster).expect("cache entry exists"));
        }

        // Evict all entries when the cache fills up.
        if self.max_cache > 0 && self.cache.len() >= self.max_cache {
            self.cache.clear();
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

        // Read and decompress in a block so the immutable borrow from at()
        // is released before we mutate self.cache.
        let (data, extended) = {
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
            (data, extended)
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

    fn cluster_offset(&self, cluster: u32) -> Result<u64> {
        let b = self.at(self.hdr.cluster_ptr_pos + 8 * u64::from(cluster), 8)?;
        Ok(le_u64(b))
    }

    fn at(&self, off: u64, n: usize) -> Result<&[u8]> {
        let start = off as usize;
        let end = start.saturating_add(n);
        let data = self.data.as_slice();
        if off > self.size || end > data.len() {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        Ok(&data[start..end])
    }

    fn at_clamped(&self, off: u64, n: usize) -> Result<&[u8]> {
        let start = off as usize;
        if off > self.size {
            return Err(Error::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        let data = self.data.as_slice();
        let end = start.saturating_add(n).min(data.len());
        Ok(&data[start..end])
    }
}

// Private methods on &self (no mutation needed)
impl Reader {
    fn dirent_at_index(&self, idx: u32) -> Result<Dirent> {
        let pb = self.at(self.hdr.url_ptr_pos + 8 * u64::from(idx), 8)?;
        self.dirent_at(le_u64(pb))
    }

    fn dirent_at(&self, off: u64) -> Result<Dirent> {
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
            let Some((url, n1)) = read_c_string(b, p) else {
                if window >= 1 << 20 || off + window as u64 >= self.size {
                    return Err(Error::UnterminatedUrl(off));
                }
                window *= 4;
                continue;
            };
            let Some((title, _)) = read_c_string(b, n1) else {
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

    fn dirent_at_title_index(&self, title_idx: u32) -> Result<Dirent> {
        let title_ptr_size = (self.hdr.cluster_ptr_pos - self.hdr.title_ptr_pos) as usize;
        let ptrs = self.at(self.hdr.title_ptr_pos, title_ptr_size)?;
        let url_idx = le_u32(&ptrs[4 * title_idx as usize..4 * (title_idx as usize + 1)]);
        self.dirent_at_index(url_idx)
    }

    fn title_index_to_url_index(&self, title_idx: u32) -> Result<u32> {
        let title_ptr_size = (self.hdr.cluster_ptr_pos - self.hdr.title_ptr_pos) as usize;
        let ptrs = self.at(self.hdr.title_ptr_pos, title_ptr_size)?;
        Ok(le_u32(
            &ptrs[4 * title_idx as usize..4 * (title_idx as usize + 1)],
        ))
    }

    /// Returns the first title index where key >= target.
    fn lower_bound_title(&self, target: &str, title_ptr_size: usize) -> Result<u32> {
        let count = self.hdr.article_count;
        let ptrs = self.at(self.hdr.title_ptr_pos, title_ptr_size)?;
        let mut lo = 0u32;
        let mut hi = count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let url_idx = le_u32(&ptrs[4 * mid as usize..4 * (mid as usize + 1)]);
            let d = self.dirent_at_index(url_idx)?;
            match key(d.namespace, &d.title).as_str().cmp(target) {
                std::cmp::Ordering::Less => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Ok(lo)
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
