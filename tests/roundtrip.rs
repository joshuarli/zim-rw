use std::io::Cursor;
use zim::{Reader, Writer, NAMESPACE_CONTENT, NAMESPACE_METADATA, NAMESPACE_WELL_KNOWN};

fn build_sample(no_compress: bool) -> Vec<u8> {
    let mut w = Writer::new();
    w.set_no_compress(no_compress);
    w.add_content(
        NAMESPACE_CONTENT,
        "index.html",
        "Home",
        "text/html",
        format!("<h1>Home</h1>{}", " word".repeat(500)).into_bytes(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "about/index.html",
        "About",
        "text/html",
        b"<h1>About</h1>".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "_k/h/logo.png",
        "",
        "image/png",
        vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3, 4, 5],
    );
    w.add_metadata("Title", "Sample");
    w.add_metadata("Language", "eng");
    w.add_redirect(
        NAMESPACE_WELL_KNOWN,
        "mainPage",
        "Main",
        NAMESPACE_CONTENT,
        "index.html",
    );
    w.set_main_page(NAMESPACE_CONTENT, "index.html");

    let mut buf = Vec::new();
    let n = w.write_to(&mut buf).expect("write archive");
    assert_eq!(n as usize, buf.len());
    buf
}

#[test]
fn round_trip() {
    for no_compress in [false, true] {
        let data = build_sample(no_compress);
        let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");

        let home = r.get(NAMESPACE_CONTENT, "index.html").expect("get home");
        assert!(String::from_utf8_lossy(&home.data).starts_with("<h1>Home</h1>"));
        assert_eq!(home.mime_type, "text/html");

        let logo = r
            .get(NAMESPACE_CONTENT, "_k/h/logo.png")
            .expect("get logo");
        assert_eq!(logo.data, vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3, 4, 5]);
        assert_eq!(logo.mime_type, "image/png");

        let meta = r.get(NAMESPACE_METADATA, "Title").expect("get metadata");
        assert_eq!(meta.data, b"Sample");

        let red = r
            .get(NAMESPACE_WELL_KNOWN, "mainPage")
            .expect("get redirect");
        assert!(String::from_utf8_lossy(&red.data).starts_with("<h1>Home</h1>"));

        let mp = r.main_page().expect("main page");
        assert!(String::from_utf8_lossy(&mp.data).starts_with("<h1>Home</h1>"));

        assert!(r.get(NAMESPACE_CONTENT, "nope.html").is_err());
    }
}

#[test]
fn entry_iteration_preserves_redirects() {
    let data = build_sample(true);
    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");
    let mut saw_redirect = false;

    for idx in 0..r.count() {
        let entry = r.entry_at(idx).expect("entry");
        if entry.namespace == NAMESPACE_WELL_KNOWN && entry.url == "mainPage" {
            saw_redirect = true;
            assert!(entry.redirect);
            assert_eq!(entry.redirect_namespace, NAMESPACE_CONTENT);
            assert_eq!(entry.redirect_url, "index.html");
            assert!(entry.data.is_empty());
        }
    }

    assert!(saw_redirect);
}

#[test]
fn checksum() {
    let data = build_sample(false);
    assert!(data.len() >= 16);
    let (body, sum) = data.split_at(data.len() - 16);
    assert_eq!(sum, md5::compute(body).0);

    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");
    assert!(r.check().expect("check archive"));

    let mut bad = data;
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    let mut r = Reader::new(Cursor::new(bad.clone()), bad.len() as u64).expect("new reader");
    assert!(!r.check().expect("check archive"));
}

#[test]
fn deterministic() {
    let a = build_sample(false);
    let b = build_sample(false);
    assert_eq!(a, b);
}

#[test]
fn magic_and_header() {
    let data = build_sample(false);
    assert_eq!(
        u32::from_le_bytes(data[0..4].try_into().unwrap()),
        zim::MAGIC
    );
    let article_count = u32::from_le_bytes(data[24..28].try_into().unwrap());
    let main_page = u32::from_le_bytes(data[64..68].try_into().unwrap());
    let checksum_pos = u64::from_le_bytes(data[72..80].try_into().unwrap());
    assert_eq!(checksum_pos, (data.len() - 16) as u64);
    assert_eq!(article_count, 6);
    assert_ne!(main_page, u32::MAX);
}

#[test]
fn unicode_and_empty_blobs_round_trip() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "L\u{fc}liang",
        "\u{dc}bersicht",
        "text/plain",
        Vec::new(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write archive");
    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");
    let blob = r.get(NAMESPACE_CONTENT, "L\u{fc}liang").expect("get blob");
    assert_eq!(blob.title, "\u{dc}bersicht");
    assert!(blob.data.is_empty());
}

#[test]
fn duplicate_content_replaces_previous_entry() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "same",
        "Old",
        "text/plain",
        b"old".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "same",
        "New",
        "text/plain",
        b"new".to_vec(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write archive");
    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");

    assert_eq!(r.count(), 1);
    let blob = r.get(NAMESPACE_CONTENT, "same").expect("get blob");
    assert_eq!(blob.title, "New");
    assert_eq!(blob.data, b"new");
}

#[test]
fn missing_redirect_target_is_write_error() {
    let mut w = Writer::new();
    w.add_redirect(
        NAMESPACE_WELL_KNOWN,
        "mainPage",
        "Main",
        NAMESPACE_CONTENT,
        "missing",
    );

    let mut data = Vec::new();
    let err = w
        .write_to(&mut data)
        .expect_err("redirect target should fail");
    assert!(err.to_string().contains("missing target"));
}

#[test]
fn invalid_headers_return_errors() {
    let data = build_sample(true);

    assert!(Reader::new(Cursor::new(vec![0; 8]), 8).is_err());

    let mut bad_magic = data.clone();
    bad_magic[0] = 0;
    assert!(Reader::new(Cursor::new(bad_magic.clone()), bad_magic.len() as u64).is_err());

    let mut bad_version = data;
    bad_version[4..6].copy_from_slice(&7u16.to_le_bytes());
    assert!(Reader::new(Cursor::new(bad_version.clone()), bad_version.len() as u64).is_err());
}

#[test]
fn truncated_archive_lookup_returns_error() {
    let mut data = build_sample(true);
    let checksum_pos = u64::from_le_bytes(data[72..80].try_into().unwrap()) as usize;
    data.truncate(checksum_pos - 1);
    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");

    assert!(r.get(NAMESPACE_CONTENT, "index.html").is_err());
}

#[test]
fn reads_extended_cluster_offsets() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "a",
        "A",
        "application/octet-stream",
        b"alpha".to_vec(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write archive");

    let cluster_ptr_pos = u64::from_le_bytes(data[48..56].try_into().unwrap()) as usize;
    let cluster_pos = u64::from_le_bytes(
        data[cluster_ptr_pos..cluster_ptr_pos + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    let checksum_pos = u64::from_le_bytes(data[72..80].try_into().unwrap()) as usize;
    let body = data[cluster_pos + 1..checksum_pos].to_vec();
    assert_eq!(data[cluster_pos], 1);
    assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 8);
    assert_eq!(u32::from_le_bytes(body[4..8].try_into().unwrap()), 13);

    let mut extended = Vec::new();
    extended.push(0x11);
    extended.extend_from_slice(&16u64.to_le_bytes());
    extended.extend_from_slice(&21u64.to_le_bytes());
    extended.extend_from_slice(&body[8..]);

    data.splice(cluster_pos..checksum_pos, extended);
    let new_checksum_pos = checksum_pos + 8;
    data[72..80].copy_from_slice(&(new_checksum_pos as u64).to_le_bytes());
    let digest = md5::compute(&data[..new_checksum_pos]).0;
    data[new_checksum_pos..].copy_from_slice(&digest);

    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");
    let blob = r.get(NAMESPACE_CONTENT, "a").expect("get extended blob");
    assert_eq!(blob.data, b"alpha");
}

#[test]
fn reads_old_zero_compression_as_uncompressed() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "a",
        "A",
        "application/octet-stream",
        b"alpha".to_vec(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write archive");
    let cluster_ptr_pos = u64::from_le_bytes(data[48..56].try_into().unwrap()) as usize;
    let cluster_pos = u64::from_le_bytes(
        data[cluster_ptr_pos..cluster_ptr_pos + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    data[cluster_pos] = 0;
    let checksum_pos = u64::from_le_bytes(data[72..80].try_into().unwrap()) as usize;
    let digest = md5::compute(&data[..checksum_pos]).0;
    data[checksum_pos..].copy_from_slice(&digest);

    let mut r = Reader::new(Cursor::new(data.clone()), data.len() as u64).expect("new reader");
    let blob = r.get(NAMESPACE_CONTENT, "a").expect("get blob");
    assert_eq!(blob.data, b"alpha");
}
