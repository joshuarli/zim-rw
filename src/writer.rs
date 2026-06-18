use crate::codec::{is_text_mime, zstd_encode};
use crate::format::{
    md5, put_u16, put_u32, put_u64, Header, COMP_NONE, COMP_ZSTD, HEADER_LEN, NAMESPACE_METADATA,
    NO_MAIN_PAGE, REDIRECT_ENTRY,
};
use crate::reader::make_key as key;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write;

const MAX_CLUSTER_CONTENT: usize = 2 << 20;

type Compressor = dyn Fn(&[u8]) -> Result<Vec<u8>> + Send + Sync;

/// Accumulates entries and serializes them as a ZIM file.
pub struct Writer {
    entries: Vec<WriterEntry>,
    by_key: HashMap<String, usize>,
    main_key: String,
    no_compress: bool,
    compress: Box<Compressor>,
}

#[derive(Clone, Debug)]
struct WriterEntry {
    namespace: u8,
    url: String,
    title: String,
    mime: String,
    data: Vec<u8>,
    redirect: bool,
    target_key: String,
    mime_idx: u16,
    cluster: u32,
    blob: u32,
    target_index: u32,
    url_index: u32,
    position: u64,
}

#[derive(Default)]
struct Plan {
    hdr: Header,
    mime_list: Vec<u8>,
    url_ptrs: Vec<u8>,
    title_ptrs: Vec<u8>,
    cluster_ptrs: Vec<u8>,
    dirents: Vec<Vec<u8>>,
    clusters: Vec<Vec<u8>>,
}

struct ClusterBuf {
    comp: u8,
    blobs: Vec<Vec<u8>>,
    size: usize,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// Returns an empty writer.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_key: HashMap::new(),
            main_key: String::new(),
            no_compress: false,
            compress: Box::new(zstd_encode),
        }
    }

    /// Stores every cluster uncompressed.
    pub fn set_no_compress(&mut self, v: bool) {
        self.no_compress = v;
    }

    /// Replaces the cluster compressor.
    ///
    /// The function must return zstd-compressed bytes because the writer marks
    /// compressed text clusters as zstd.
    pub fn set_compress<F>(&mut self, f: F)
    where
        F: Fn(&[u8]) -> Result<Vec<u8>> + Send + Sync + 'static,
    {
        self.compress = Box::new(f);
    }

    /// Restores the built-in zstd compressor.
    pub fn reset_compress(&mut self) {
        self.compress = Box::new(zstd_encode);
    }

    /// Adds or replaces a content entry.
    pub fn add_content(
        &mut self,
        namespace: u8,
        url: impl Into<String>,
        title: impl Into<String>,
        mime: impl Into<String>,
        data: impl Into<Vec<u8>>,
    ) {
        let url = url.into();
        let mut title = title.into();
        if title.is_empty() {
            title = url.clone();
        }
        let mut mime = mime.into();
        if mime.is_empty() {
            mime = "application/octet-stream".to_owned();
        }
        self.put(WriterEntry {
            namespace,
            url,
            title,
            mime,
            data: data.into(),
            redirect: false,
            target_key: String::new(),
            mime_idx: 0,
            cluster: 0,
            blob: 0,
            target_index: 0,
            url_index: 0,
            position: 0,
        });
    }

    /// Adds a text metadata entry in the `M` namespace.
    pub fn add_metadata(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.put(WriterEntry {
            namespace: NAMESPACE_METADATA,
            url: name.clone(),
            title: name,
            mime: "text/plain".to_owned(),
            data: value.into().into_bytes(),
            redirect: false,
            target_key: String::new(),
            mime_idx: 0,
            cluster: 0,
            blob: 0,
            target_index: 0,
            url_index: 0,
            position: 0,
        });
    }

    /// Adds a metadata entry with an explicit MIME type.
    pub fn add_metadata_bytes(
        &mut self,
        name: impl Into<String>,
        mime: impl Into<String>,
        data: impl Into<Vec<u8>>,
    ) {
        let name = name.into();
        let mut mime = mime.into();
        if mime.is_empty() {
            mime = "application/octet-stream".to_owned();
        }
        self.put(WriterEntry {
            namespace: NAMESPACE_METADATA,
            url: name.clone(),
            title: name,
            mime,
            data: data.into(),
            redirect: false,
            target_key: String::new(),
            mime_idx: 0,
            cluster: 0,
            blob: 0,
            target_index: 0,
            url_index: 0,
            position: 0,
        });
    }

    /// Adds a redirect from `(namespace, url)` to `(target_namespace, target_url)`.
    pub fn add_redirect(
        &mut self,
        namespace: u8,
        url: impl Into<String>,
        title: impl Into<String>,
        target_namespace: u8,
        target_url: impl Into<String>,
    ) {
        let url = url.into();
        let mut title = title.into();
        if title.is_empty() {
            title = url.clone();
        }
        let target_url = target_url.into();
        self.put(WriterEntry {
            namespace,
            url,
            title,
            mime: String::new(),
            data: Vec::new(),
            redirect: true,
            target_key: key(target_namespace, &target_url),
            mime_idx: 0,
            cluster: 0,
            blob: 0,
            target_index: 0,
            url_index: 0,
            position: 0,
        });
    }

    /// Marks an entry as the archive's entry point.
    pub fn set_main_page(&mut self, namespace: u8, url: &str) {
        self.main_key = key(namespace, url);
    }

    /// Serializes the archive to `out` and returns the number of bytes written.
    pub fn write_to(&mut self, out: &mut impl Write) -> Result<u64> {
        let p = self.build_plan()?;
        let mut body = Vec::new();
        body.extend_from_slice(&p.hdr.marshal());
        body.extend_from_slice(&p.mime_list);
        body.extend_from_slice(&p.url_ptrs);
        body.extend_from_slice(&p.title_ptrs);
        body.extend_from_slice(&p.cluster_ptrs);
        for section in p.dirents.iter().chain(p.clusters.iter()) {
            body.extend_from_slice(section);
        }
        let digest = md5(&body);
        out.write_all(&body)?;
        out.write_all(&digest)?;
        Ok((body.len() + digest.len()) as u64)
    }

    fn put(&mut self, e: WriterEntry) {
        let k = key(e.namespace, &e.url);
        if let Some(idx) = self.by_key.get(&k).copied() {
            self.entries[idx] = e;
            return;
        }
        self.by_key.insert(k, self.entries.len());
        self.entries.push(e);
    }

    fn build_plan(&mut self) -> Result<Plan> {
        let mut p = Plan::default();
        let mut ents = self.entries.clone();
        ents.sort_by_key(|a| key(a.namespace, &a.url));
        let mut index = HashMap::with_capacity(ents.len());
        for (i, e) in ents.iter_mut().enumerate() {
            e.url_index = i as u32;
            index.insert(key(e.namespace, &e.url), i as u32);
        }

        for e in &mut ents {
            if !e.redirect {
                continue;
            }
            let Some(target_index) = index.get(&e.target_key).copied() else {
                return Err(Error::MissingRedirectTarget {
                    redirect: key(e.namespace, &e.url),
                    target: e.target_key.clone(),
                });
            };
            e.target_index = target_index;
        }

        let mut mimes = Vec::new();
        let mut mime_index = HashMap::new();
        for e in &mut ents {
            if e.redirect {
                continue;
            }
            let idx = if let Some(idx) = mime_index.get(&e.mime) {
                *idx
            } else {
                let idx = mimes.len() as u16;
                mime_index.insert(e.mime.clone(), idx);
                mimes.push(e.mime.clone());
                idx
            };
            e.mime_idx = idx;
        }
        p.mime_list = encode_mime_list(&mimes);

        let clusters = self.pack_clusters(&mut ents);
        for c in &clusters {
            p.clusters
                .push(c.encode(self.no_compress, &*self.compress)?);
        }

        for e in &ents {
            p.dirents.push(e.encode_dirent());
        }

        let count = ents.len() as u32;
        let mut pos = HEADER_LEN as u64;
        let mime_list_pos = pos;
        pos += p.mime_list.len() as u64;
        let url_ptr_pos = pos;
        pos += 8 * u64::from(count);
        let title_ptr_pos = pos;
        pos += 4 * u64::from(count);
        let cluster_ptr_pos = pos;
        pos += 8 * p.clusters.len() as u64;
        for (e, dirent) in ents.iter_mut().zip(&p.dirents) {
            e.position = pos;
            pos += dirent.len() as u64;
        }
        let mut cluster_pos = Vec::with_capacity(p.clusters.len());
        for cluster in &p.clusters {
            cluster_pos.push(pos);
            pos += cluster.len() as u64;
        }
        let checksum_pos = pos;

        p.url_ptrs = vec![0; 8 * count as usize];
        for (i, e) in ents.iter().enumerate() {
            put_u64(&mut p.url_ptrs[8 * i..], e.position);
        }
        p.cluster_ptrs = vec![0; 8 * cluster_pos.len()];
        for (i, cp) in cluster_pos.iter().enumerate() {
            put_u64(&mut p.cluster_ptrs[8 * i..], *cp);
        }
        p.title_ptrs = encode_title_ptrs(&ents);

        p.hdr = Header {
            uuid: derive_uuid(&ents),
            article_count: count,
            cluster_count: p.clusters.len() as u32,
            url_ptr_pos,
            title_ptr_pos,
            cluster_ptr_pos,
            mime_list_pos,
            main_page: NO_MAIN_PAGE,
            layout_page: NO_MAIN_PAGE,
            checksum_pos,
        };
        if !self.main_key.is_empty() {
            if let Some(mi) = index.get(&self.main_key) {
                p.hdr.main_page = *mi;
            }
        }
        Ok(p)
    }

    fn pack_clusters(&self, ents: &mut [WriterEntry]) -> Vec<ClusterBuf> {
        let mut clusters = Vec::new();
        let mut cur_text = None;
        let mut cur_bin = None;

        for e in ents {
            if e.redirect {
                continue;
            }
            let (cur, comp) = if is_text_mime(&e.mime) {
                (&mut cur_text, COMP_ZSTD)
            } else {
                (&mut cur_bin, COMP_NONE)
            };
            if cur.is_none() {
                *cur = Some(clusters.len());
                clusters.push(ClusterBuf {
                    comp,
                    blobs: Vec::new(),
                    size: 0,
                });
            }
            let cluster_idx = cur.expect("cluster index assigned");
            let c = &mut clusters[cluster_idx];
            e.cluster = cluster_idx as u32;
            e.blob = c.blobs.len() as u32;
            c.size += e.data.len();
            c.blobs.push(e.data.clone());
            if c.size >= MAX_CLUSTER_CONTENT {
                *cur = None;
            }
        }
        clusters
    }
}

impl ClusterBuf {
    fn encode(&self, no_compress: bool, compress: &Compressor) -> Result<Vec<u8>> {
        let table_len = 4 * (self.blobs.len() + 1);
        let total = table_len + self.blobs.iter().map(Vec::len).sum::<usize>();
        let mut data = vec![0; table_len];
        let mut off = table_len as u32;
        put_u32(&mut data[0..], off);
        for (i, b) in self.blobs.iter().enumerate() {
            off += b.len() as u32;
            put_u32(&mut data[4 * (i + 1)..], off);
        }
        data.reserve(total - table_len);
        for b in &self.blobs {
            data.extend_from_slice(b);
        }

        let mut comp = self.comp;
        if no_compress {
            comp = COMP_NONE;
        }
        let payload = if comp == COMP_ZSTD {
            compress(&data)?
        } else {
            comp = COMP_NONE;
            data
        };
        let mut out = Vec::with_capacity(payload.len() + 1);
        out.push(comp);
        out.extend_from_slice(&payload);
        Ok(out)
    }
}

impl WriterEntry {
    fn encode_dirent(&self) -> Vec<u8> {
        let mut head;
        if self.redirect {
            head = vec![0; 12];
            put_u16(&mut head[0..], REDIRECT_ENTRY);
            head[3] = self.namespace;
            put_u32(&mut head[8..], self.target_index);
        } else {
            head = vec![0; 16];
            put_u16(&mut head[0..], self.mime_idx);
            head[3] = self.namespace;
            put_u32(&mut head[8..], self.cluster);
            put_u32(&mut head[12..], self.blob);
        }
        head.extend_from_slice(self.url.as_bytes());
        head.push(0);
        head.extend_from_slice(self.title.as_bytes());
        head.push(0);
        head
    }
}

fn encode_mime_list(mimes: &[String]) -> Vec<u8> {
    let mut b = Vec::new();
    for m in mimes {
        b.extend_from_slice(m.as_bytes());
        b.push(0);
    }
    b.push(0);
    b
}

fn encode_title_ptrs(ents: &[WriterEntry]) -> Vec<u8> {
    let mut order = ents.to_vec();
    order.sort_by(|a, b| {
        let ta = key(a.namespace, &a.title);
        let tb = key(b.namespace, &b.title);
        ta.cmp(&tb).then(a.url_index.cmp(&b.url_index))
    });
    let mut b = vec![0; 4 * order.len()];
    for (i, e) in order.iter().enumerate() {
        put_u32(&mut b[4 * i..], e.url_index);
    }
    b
}

fn derive_uuid(ents: &[WriterEntry]) -> [u8; 16] {
    let mut body = Vec::new();
    let mut n = [0; 8];
    for e in ents {
        body.extend_from_slice(key(e.namespace, &e.url).as_bytes());
        n.copy_from_slice(&(e.data.len() as u64).to_le_bytes());
        body.extend_from_slice(&n);
        body.extend_from_slice(&e.data);
    }
    md5(&body)
}

#[cfg(test)]
mod tests {
    use crate::format::md5;

    #[test]
    fn md5_known_vector() {
        assert_eq!(
            md5(b"abc"),
            [
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72
            ]
        );
    }
}
