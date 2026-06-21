use crate::codec::{is_text_mime, zstd_encode};
use crate::format::{
    md5, put_u16, put_u32, put_u64, Header, COMP_NONE, COMP_ZSTD, HEADER_LEN, NAMESPACE_METADATA,
    NO_MAIN_PAGE, REDIRECT_ENTRY,
};
use crate::reader::make_key as key;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

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
    data_len: u64,
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
            data_len: 0,
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

    /// Declares an entry without storing its data.
    ///
    /// The data will be provided later via [`write_to_streaming`] or
    /// [`write_to_file`]. `data_len` is the uncompressed byte count and
    /// must match the actual data length.
    pub fn add_entry(
        &mut self,
        namespace: u8,
        url: impl Into<String>,
        title: impl Into<String>,
        mime: impl Into<String>,
        data_len: u64,
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
            data: Vec::new(),
            data_len,
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
            data_len: 0,
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
            data_len: 0,
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
            data_len: 0,
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
    ///
    /// If `out` supports [`Seek`], use [`write_to_file`](Self::write_to_file)
    /// instead for lower memory usage.
    pub fn write_to(&mut self, out: &mut impl Write) -> Result<u64> {
        let p = self.build_plan()?;
        let mut ctx = md5::Context::new();
        let mut wrote = 0u64;

        let hdr = p.hdr.marshal();
        ctx.consume(&hdr);
        out.write_all(&hdr)?;
        wrote += hdr.len() as u64;

        ctx.consume(&p.mime_list);
        out.write_all(&p.mime_list)?;
        wrote += p.mime_list.len() as u64;

        ctx.consume(&p.url_ptrs);
        out.write_all(&p.url_ptrs)?;
        wrote += p.url_ptrs.len() as u64;

        ctx.consume(&p.title_ptrs);
        out.write_all(&p.title_ptrs)?;
        wrote += p.title_ptrs.len() as u64;

        ctx.consume(&p.cluster_ptrs);
        out.write_all(&p.cluster_ptrs)?;
        wrote += p.cluster_ptrs.len() as u64;

        for section in p.dirents.iter().chain(p.clusters.iter()) {
            ctx.consume(section);
            out.write_all(section)?;
            wrote += section.len() as u64;
        }

        let digest = ctx.finalize().0;
        out.write_all(&digest)?;
        wrote += 16;
        Ok(wrote)
    }

    /// Serializes the archive to a seekable output using streaming writes.
    ///
    /// Content data is read on demand via `data_provider(idx)`, which receives
    /// the entry index in URL-sorted order. This avoids buffering all entry data
    /// in memory.
    ///
    /// `num_threads` controls how many clusters are compressed in parallel.
    /// Pass 0 to use the CPU core count.
    pub fn write_to_streaming(
        &mut self,
        out: &mut (impl Read + Write + Seek),
        mut data_provider: impl FnMut(usize) -> Result<Vec<u8>>,
        num_threads: usize,
    ) -> Result<u64> {
        let num_threads = if num_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            num_threads
        };

        let (plan, cluster_entries) = self.build_plan_metadata()?;
        if cluster_entries.is_empty() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "archive has no content entries",
            )));
        }

        let metadata_end = plan_metadata_size(&plan, cluster_entries.len());

        // Write placeholder metadata (header + sections with zero cluster ptrs)
        write_metadata_placeholder(out, &plan, cluster_entries.len(), metadata_end)?;

        // Build and write clusters, recording their actual positions
        let cluster_positions = write_clusters(
            out,
            metadata_end,
            &cluster_entries,
            &plan.entries,
            &mut data_provider,
            self.no_compress,
            num_threads,
        )?;

        let checksum_pos = cluster_positions
            .last()
            .map(|(_, end)| *end)
            .unwrap_or(metadata_end);

        // Backpatch cluster pointer list
        let cluster_ptr_pos = plan.cluster_ptr_pos;
        out.seek(SeekFrom::Start(cluster_ptr_pos))?;
        let mut ptrs = vec![0u8; 8 * cluster_positions.len()];
        for (i, &(start, _)) in cluster_positions.iter().enumerate() {
            put_u64(&mut ptrs[8 * i..], start);
        }
        out.write_all(&ptrs)?;

        // Backpatch header with checksum_pos
        out.seek(SeekFrom::Start(0))?;
        let mut hdr = plan.hdr;
        hdr.checksum_pos = checksum_pos;
        out.write_all(&hdr.marshal())?;

        // Compute MD5 by reading back the file up to checksum_pos
        out.seek(SeekFrom::Start(0))?;
        let digest = stream_md5(out, checksum_pos)?;
        out.seek(SeekFrom::Start(checksum_pos))?;
        out.write_all(&digest)?;

        Ok(checksum_pos + 16)
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

    fn entry_size(&self, e: &WriterEntry) -> usize {
        if e.data.is_empty() && e.data_len > 0 {
            e.data_len as usize
        } else {
            e.data.len()
        }
    }

    fn build_plan(&mut self) -> Result<Plan> {
        let (plan, cluster_entries) = self.build_plan_metadata()?;

        // Build cluster data from stored entries
        let mut clusters = Vec::with_capacity(cluster_entries.len());
        for ce in &cluster_entries {
            let blobs: Vec<Vec<u8>> = ce
                .blob_indices
                .iter()
                .map(|&bi| {
                    let e = &plan.entries[ce.entry_indices[bi]];
                    if e.data.is_empty() && e.data_len > 0 {
                        return Err(Error::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("entry '{}' has no stored data; use write_to_streaming", e.url),
                        )));
                    }
                    Ok(e.data.clone())
                })
                .collect::<Result<Vec<_>>>()?;

            let cb = ClusterBuf {
                comp: ce.comp,
                blobs,
            };
            clusters.push(cb.encode(self.no_compress, &*self.compress)?);
        }

        // Compute cluster pointer table from actual encoded sizes
        let metadata_end = plan_metadata_size(&plan, cluster_entries.len());
        let mut pos = metadata_end;
        let mut cluster_ptrs = vec![0u8; 8 * clusters.len()];
        for (i, c) in clusters.iter().enumerate() {
            put_u64(&mut cluster_ptrs[8 * i..], pos);
            pos += c.len() as u64;
        }
        let checksum_pos = pos;

        let mut hdr = plan.hdr;
        hdr.checksum_pos = checksum_pos;

        Ok(Plan {
            hdr,
            mime_list: plan.mime_list,
            url_ptrs: plan.url_ptrs,
            title_ptrs: plan.title_ptrs,
            cluster_ptrs,
            dirents: plan.dirents,
            clusters,
        })
    }

    fn build_plan_metadata(&mut self) -> Result<(PlanMetadata, Vec<ClusterEntry>)> {
        let mut plan = PlanMetadata::default();
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
        plan.mime_list = encode_mime_list(&mimes);

        let cluster_entries = pack_clusters(&ents, self);
        for (ci, ce) in cluster_entries.iter().enumerate() {
            for (bi, &ei) in ce.blob_indices.iter().enumerate() {
                ents[ce.entry_indices[ei]].cluster = ci as u32;
                ents[ce.entry_indices[ei]].blob = bi as u32;
            }
        }

        for e in &ents {
            plan.dirents.push(e.encode_dirent());
        }

        let count = ents.len() as u32;
        let mut pos = HEADER_LEN as u64;
        let mime_list_pos = pos;
        pos += plan.mime_list.len() as u64;
        let url_ptr_pos = pos;
        pos += 8 * u64::from(count);
        let title_ptr_pos = pos;
        pos += 4 * u64::from(count);
        let cluster_ptr_pos = pos;
        pos += 8 * cluster_entries.len() as u64;
        for (e, dirent) in ents.iter_mut().zip(&plan.dirents) {
            e.position = pos;
            pos += dirent.len() as u64;
        }

        plan.url_ptrs = vec![0; 8 * count as usize];
        for (i, e) in ents.iter().enumerate() {
            put_u64(&mut plan.url_ptrs[8 * i..], e.position);
        }
        plan.title_ptrs = encode_title_ptrs(&ents);

        plan.hdr = Header {
            uuid: derive_uuid(&ents),
            article_count: count,
            cluster_count: cluster_entries.len() as u32,
            url_ptr_pos,
            title_ptr_pos,
            cluster_ptr_pos,
            mime_list_pos,
            main_page: NO_MAIN_PAGE,
            layout_page: NO_MAIN_PAGE,
            checksum_pos: 0, // filled later
        };
        if !self.main_key.is_empty() {
            if let Some(mi) = index.get(&self.main_key) {
                plan.hdr.main_page = *mi;
            }
        }
        plan.cluster_ptr_pos = cluster_ptr_pos;
        plan.entries = ents;

        Ok((plan, cluster_entries))
    }
}

#[derive(Default)]
struct PlanMetadata {
    hdr: Header,
    mime_list: Vec<u8>,
    url_ptrs: Vec<u8>,
    title_ptrs: Vec<u8>,
    dirents: Vec<Vec<u8>>,
    cluster_ptr_pos: u64,
    entries: Vec<WriterEntry>,
}

struct ClusterEntry {
    comp: u8,
    /// Indices into `PlanMetadata.entries`.
    entry_indices: Vec<usize>,
    /// Indices into `entry_indices` for each blob in this cluster.
    blob_indices: Vec<usize>,
}

fn pack_clusters(ents: &[WriterEntry], writer: &Writer) -> Vec<ClusterEntry> {
    let mut clusters: Vec<ClusterEntry> = Vec::new();
    let mut cur_text: Option<usize> = None;
    let mut cur_bin: Option<usize> = None;

    for (ei, e) in ents.iter().enumerate() {
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
            clusters.push(ClusterEntry {
                comp,
                entry_indices: Vec::new(),
                blob_indices: Vec::new(),
            });
        }
        let ci = cur.expect("cluster index assigned");
        let ce = &mut clusters[ci];
        let blob_idx = ce.blob_indices.len();
        ce.blob_indices.push(blob_idx);
        ce.entry_indices.push(ei);
        let current: usize = ce.blob_indices.iter().map(|&i| writer.entry_size(&ents[ce.entry_indices[i]])).sum();
        if current >= MAX_CLUSTER_CONTENT {
            *cur = None;
        }
    }
    clusters
}

fn plan_metadata_size(plan: &PlanMetadata, cluster_count: usize) -> u64 {
    let mut pos = HEADER_LEN as u64;
    pos += plan.mime_list.len() as u64;
    pos += 8 * u64::from(plan.hdr.article_count); // url ptrs
    pos += 4 * u64::from(plan.hdr.article_count); // title ptrs
    pos += 8 * cluster_count as u64; // cluster ptrs
    for d in &plan.dirents {
        pos += d.len() as u64;
    }
    pos
}

fn write_metadata_placeholder(
    out: &mut (impl Write + Seek),
    plan: &PlanMetadata,
    cluster_count: usize,
    metadata_end: u64,
) -> Result<()> {
    out.seek(SeekFrom::Start(0))?;

    // Write header placeholder (checksum_pos = 0 for now)
    out.write_all(&plan.hdr.marshal())?;
    out.write_all(&plan.mime_list)?;
    out.write_all(&plan.url_ptrs)?;
    out.write_all(&plan.title_ptrs)?;

    // Write zero-filled cluster pointers
    let zero_ptrs = vec![0u8; 8 * cluster_count];
    out.write_all(&zero_ptrs)?;

    for d in &plan.dirents {
        out.write_all(d)?;
    }

    // Zero-fill any gap between metadata end and reserved end
    let current = out.stream_position()?;
    if current < metadata_end {
        let gap = vec![0u8; (metadata_end - current) as usize];
        out.write_all(&gap)?;
    }

    Ok(())
}

fn write_clusters(
    out: &mut (impl Write + Seek),
    metadata_end: u64,
    cluster_entries: &[ClusterEntry],
    entries: &[WriterEntry],
    data_provider: &mut impl FnMut(usize) -> Result<Vec<u8>>,
    no_compress: bool,
    num_threads: usize,
) -> Result<Vec<(u64, u64)>> {
    use std::collections::VecDeque;

    out.seek(SeekFrom::Start(metadata_end))?;
    let mut positions = Vec::with_capacity(cluster_entries.len());
    let mut current = metadata_end;
    let mut handles: VecDeque<std::thread::JoinHandle<Result<Vec<u8>>>> = VecDeque::new();

    for (_ci, ce) in cluster_entries.iter().enumerate() {
        // Build raw cluster data: offset table + concatenated blobs
        let raw = build_raw_cluster(ce, entries, data_provider)?;
        let comp = if no_compress { COMP_NONE } else { ce.comp };

        let handle = std::thread::spawn(move || encode_cluster_data(comp, raw));
        handles.push_back(handle);

        if handles.len() >= num_threads {
            let encoded = handles
                .pop_front()
                .unwrap()
                .join()
                .map_err(|_| Error::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "compression thread panicked",
                )))??;
            let start = current;
            out.write_all(&encoded)?;
            current += encoded.len() as u64;
            positions.push((start, current));
        }
    }

    for handle in handles {
        let encoded = handle
            .join()
            .map_err(|_| Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "compression thread panicked",
            )))??;
        let start = current;
        out.write_all(&encoded)?;
        current += encoded.len() as u64;
        positions.push((start, current));
    }

    Ok(positions)
}

fn build_raw_cluster(
    ce: &ClusterEntry,
    entries: &[WriterEntry],
    data_provider: &mut impl FnMut(usize) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    let num_blobs = ce.entry_indices.len();
    let table_len = 4 * (num_blobs + 1);
    let mut sizes = Vec::with_capacity(num_blobs);
    let mut total = table_len;

    for &ei in &ce.entry_indices {
        let e = &entries[ei];
        let size = if e.data.is_empty() && e.data_len > 0 {
            e.data_len as usize
        } else {
            e.data.len()
        };
        sizes.push(size);
        total += size;
    }

    let mut data = vec![0u8; total];
    put_u32(&mut data[0..4], table_len as u32);
    let mut off = table_len as u32;
    for (i, &size) in sizes.iter().enumerate() {
        off += size as u32;
        put_u32(&mut data[4 * (i + 1)..4 * (i + 2)], off);
    }

    for (bi, &ei) in ce.entry_indices.iter().enumerate() {
        let e = &entries[ei];
        let blob = if e.data.is_empty() && e.data_len > 0 {
            data_provider(ei)?
        } else {
            e.data.clone()
        };
        let start = table_len + sizes[..bi].iter().sum::<usize>();
        data[start..start + blob.len()].copy_from_slice(&blob);
    }

    Ok(data)
}

fn encode_cluster_data(comp: u8, raw: Vec<u8>) -> Result<Vec<u8>> {
    let payload = if comp == COMP_ZSTD {
        zstd_encode(&raw)?
    } else {
        raw
    };
    let mut out = Vec::with_capacity(payload.len() + 1);
    out.push(comp);
    out.extend_from_slice(&payload);
    Ok(out)
}

fn stream_md5(out: &mut (impl Read + Write + Seek), checksum_pos: u64) -> Result<[u8; 16]> {
    let mut ctx = md5::Context::new();
    let mut remaining = checksum_pos;
    // 8 MiB buffer keeps syscall count low for large archives
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    while remaining > 0 {
        let n = remaining.min(buf.len() as u64) as usize;
        let chunk = &mut buf[..n];
        out.read_exact(chunk)?;
        ctx.consume(chunk);
        remaining -= n as u64;
    }
    Ok(ctx.finalize().0)
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
        let len = if e.data.is_empty() && e.data_len > 0 {
            e.data_len
        } else {
            e.data.len() as u64
        };
        n.copy_from_slice(&len.to_le_bytes());
        body.extend_from_slice(&n);
        if !e.data.is_empty() {
            body.extend_from_slice(&e.data);
        }
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
