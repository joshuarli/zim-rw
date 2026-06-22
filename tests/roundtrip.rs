use zim::{NAMESPACE_CONTENT, NAMESPACE_METADATA, NAMESPACE_WELL_KNOWN, Reader, Writer};

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
        let mut r = Reader::from_bytes(data.clone()).expect("new reader");

        let home = r.get(NAMESPACE_CONTENT, "index.html").expect("get home");
        assert!(String::from_utf8_lossy(&home.data).starts_with("<h1>Home</h1>"));
        assert_eq!(home.mime_type, "text/html");

        let logo = r.get(NAMESPACE_CONTENT, "_k/h/logo.png").expect("get logo");
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
    let mut r = Reader::from_bytes(data.clone()).expect("new reader");
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

    let r = Reader::from_bytes(data.clone()).expect("new reader");
    assert!(r.check().expect("check archive"));

    let mut bad = data;
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    let r = Reader::from_bytes(bad.clone()).expect("new reader");
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
    assert_eq!(article_count, 8); // 3 content + 2 metadata + 1 redirect + Counter + listing
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
    let mut r = Reader::from_bytes(data.clone()).expect("new reader");
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
    let mut r = Reader::from_bytes(data.clone()).expect("new reader");

    assert_eq!(r.count(), 3); // 1 content + Counter + listing
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

    assert!(Reader::from_bytes(vec![0; 8]).is_err());

    let mut bad_magic = data.clone();
    bad_magic[0] = 0;
    assert!(Reader::from_bytes(bad_magic.clone()).is_err());

    let mut bad_version = data;
    bad_version[4..6].copy_from_slice(&7u16.to_le_bytes());
    assert!(Reader::from_bytes(bad_version.clone()).is_err());
}

#[test]
fn truncated_archive_lookup_returns_error() {
    let mut data = build_sample(true);
    // Truncate just past the dirents so the cluster pointer table and header
    // are intact, but cluster data is missing.
    let cluster_ptr_pos = u64::from_le_bytes(data[48..56].try_into().unwrap()) as usize;
    let c0_start = u64::from_le_bytes(
        data[cluster_ptr_pos..cluster_ptr_pos + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    // Truncate right at the start of the first cluster — the pointer table
    // points there but there's no data.
    data.truncate(c0_start);
    let mut r = Reader::from_bytes(data.clone()).expect("new reader");

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
    // Read the first two cluster pointers to get cluster 0 bounds
    let c0_start = u64::from_le_bytes(
        data[cluster_ptr_pos..cluster_ptr_pos + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    let cluster_count = u32::from_le_bytes(data[28..32].try_into().unwrap());
    let c0_end = if cluster_count > 1 {
        u64::from_le_bytes(
            data[cluster_ptr_pos + 8..cluster_ptr_pos + 16]
                .try_into()
                .unwrap(),
        ) as usize
    } else {
        u64::from_le_bytes(data[72..80].try_into().unwrap()) as usize
    };
    let body = data[c0_start + 1..c0_end].to_vec();
    assert_eq!(data[c0_start], 1);
    assert_eq!(u32::from_le_bytes(body[0..4].try_into().unwrap()), 8);
    assert_eq!(u32::from_le_bytes(body[4..8].try_into().unwrap()), 13);

    let mut extended = Vec::new();
    extended.push(0x11);
    extended.extend_from_slice(&16u64.to_le_bytes());
    extended.extend_from_slice(&21u64.to_le_bytes());
    extended.extend_from_slice(&body[8..]);

    let old_len = c0_end - c0_start;
    let new_len = extended.len();
    data.splice(c0_start..c0_end, extended);
    let diff = new_len as i64 - old_len as i64;

    // Update cluster pointer for cluster 1 onwards
    if cluster_count > 1 {
        for ci in 1..cluster_count as usize {
            let ptr_off = cluster_ptr_pos + 8 * ci;
            let old_pos = u64::from_le_bytes(data[ptr_off..ptr_off + 8].try_into().unwrap());
            let new_pos = (old_pos as i64 + diff) as u64;
            data[ptr_off..ptr_off + 8].copy_from_slice(&new_pos.to_le_bytes());
        }
    }

    let checksum_pos = u64::from_le_bytes(data[72..80].try_into().unwrap()) as usize;
    let new_checksum_pos = (checksum_pos as i64 + diff) as usize;
    data[72..80].copy_from_slice(&(new_checksum_pos as u64).to_le_bytes());
    let digest = md5::compute(&data[..new_checksum_pos]).0;
    data[new_checksum_pos..].copy_from_slice(&digest);

    let mut r = Reader::from_bytes(data.clone()).expect("new reader");
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

    let mut r = Reader::from_bytes(data.clone()).expect("new reader");
    let blob = r.get(NAMESPACE_CONTENT, "a").expect("get blob");
    assert_eq!(blob.data, b"alpha");
}

#[test]
fn title_lookup_works() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "index.html",
        "Home Page",
        "text/html",
        b"<h1>Home</h1>".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "about.html",
        "About Us",
        "text/html",
        b"<h1>About</h1>".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "contact.html",
        "Contact",
        "text/html",
        b"<h1>Contact</h1>".to_vec(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write");

    let mut r = Reader::from_bytes(data.clone()).expect("reader");

    let blob = r.get_by_title(0, "About Us").expect("get_by_title");
    assert_eq!(blob.url, "about.html");
    assert_eq!(blob.data, b"<h1>About</h1>");

    let blob = r
        .get_by_title(NAMESPACE_CONTENT, "Home Page")
        .expect("namespace filtered");
    assert_eq!(blob.url, "index.html");

    let results = r.entries_by_title_prefix(0, "About").expect("prefix");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "about.html");

    let results = r.entries_by_title_prefix(0, "Z").expect("no match");
    assert!(results.is_empty());

    assert!(r.get_by_title(0, "Nonexistent").is_err());
}

#[test]
fn counter_and_listing_are_generated() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "a.html",
        "Alpha",
        "text/html",
        b"a".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "b.png",
        "Beta",
        "image/png",
        b"b".to_vec(),
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write");

    let mut r = Reader::from_bytes(data.clone()).expect("reader");

    // M/Counter should be auto-generated
    let counter = r.get(NAMESPACE_METADATA, "Counter").expect("Counter");
    let text = String::from_utf8_lossy(&counter.data);
    assert!(text.contains("image/png=1"), "got: {text}");
    assert!(text.contains("text/html=1"), "got: {text}");
    assert!(
        text.contains("text/plain="),
        "listing should have text/plain count: {text}"
    );

    // X/listing/titleOrdered/v1 should be auto-generated
    let listing = r
        .get(zim::NAMESPACE_LISTING, "listing/titleOrdered/v1")
        .expect("listing");
    let text = String::from_utf8_lossy(&listing.data);
    assert!(text.contains("Alpha\n"), "got: {text}");
    assert!(text.contains("Beta\n"), "got: {text}");
}

#[test]
fn illustration_round_trips() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_illustration(48, 48, 1, vec![1, 2, 3, 4]);

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write");

    let mut r = Reader::from_bytes(data.clone()).expect("reader");
    let blob = r
        .get(NAMESPACE_METADATA, "Illustration_48x48@1")
        .expect("illustration");
    assert_eq!(blob.data, vec![1, 2, 3, 4]);
    assert_eq!(blob.mime_type, "image/png");
}

#[test]
fn cache_limit_api() {
    // Build a small archive with two content entries that land in separate
    // clusters (text vs binary) so we can test eviction.
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "index.html",
        "Home",
        "text/html",
        b"<h1>Home</h1>".to_vec(),
    );
    w.add_content(
        NAMESPACE_CONTENT,
        "logo.png",
        "",
        "image/png",
        vec![0x89, b'P', b'N', b'G', 0, 1],
    );

    let mut data = Vec::new();
    w.write_to(&mut data).expect("write");

    let mut r = Reader::from_bytes(data.clone()).expect("reader");

    assert_eq!(r.cache_limit(), 64);
    r.set_cache_limit(1);
    assert_eq!(r.cache_limit(), 1);

    // Read HTML (text cluster), then binary (binary cluster).  The second
    // read triggers eviction of the first cluster.
    let blob = r.get(NAMESPACE_CONTENT, "index.html").expect("index");
    assert_eq!(blob.data, b"<h1>Home</h1>");

    let blob = r.get(NAMESPACE_CONTENT, "logo.png").expect("logo");
    assert_eq!(blob.data, vec![0x89, b'P', b'N', b'G', 0, 1]);

    // Re-read first page — must survive eviction and return correct data
    let blob = r.get(NAMESPACE_CONTENT, "index.html").expect("index again");
    assert_eq!(blob.data, b"<h1>Home</h1>");

    // Unbounded (0) must also work
    r.set_cache_limit(0);
    assert_eq!(r.cache_limit(), 0);
    let blob = r
        .get(NAMESPACE_CONTENT, "logo.png")
        .expect("logo unbounded");
    assert_eq!(blob.data, vec![0x89, b'P', b'N', b'G', 0, 1]);
}

#[test]
fn get_range_compressed() {
    let data = build_sample(false);
    let mut r = Reader::from_bytes(data).expect("reader");

    // Read a sub-range of the HTML content
    let (slice, total) = r
        .get_range(NAMESPACE_CONTENT, "index.html", 4, 10)
        .expect("get_range");
    let home_bytes: Vec<u8> = format!("<h1>Home</h1>{}", " word".repeat(500)).into_bytes();
    assert_eq!(total as usize, home_bytes.len());
    assert_eq!(slice, &home_bytes[4..14]);

    // Read an empty range (zero length)
    let (slice, total) = r
        .get_range(NAMESPACE_CONTENT, "index.html", 0, 0)
        .expect("empty range");
    assert_eq!(total as usize, home_bytes.len());
    assert!(slice.is_empty());

    // Read from the end of the blob
    let (slice, _) = r
        .get_range(
            NAMESPACE_CONTENT,
            "index.html",
            home_bytes.len() as u64 - 5,
            100,
        )
        .expect("range from end");
    assert_eq!(slice.len(), 5);
    assert_eq!(slice, &home_bytes[home_bytes.len() - 5..]);
}

#[test]
fn get_range_uncompressed() {
    let mut w = Writer::new();
    w.set_no_compress(true);
    w.add_content(
        NAMESPACE_CONTENT,
        "data.bin",
        "",
        "application/octet-stream",
        (0u8..200).collect::<Vec<u8>>(),
    );

    let mut buf = Vec::new();
    w.write_to(&mut buf).expect("write");
    let mut r = Reader::from_bytes(buf).expect("reader");

    let (slice, total) = r
        .get_range(NAMESPACE_CONTENT, "data.bin", 10, 20)
        .expect("get_range");
    assert_eq!(total, 200);
    assert_eq!(slice.len(), 20);
    assert_eq!(slice, (10u8..30).collect::<Vec<u8>>());
}

#[test]
fn get_range_through_redirect() {
    let data = build_sample(false);
    let mut r = Reader::from_bytes(data).expect("reader");

    // W/mainPage redirects to C/index.html
    let (slice, total) = r
        .get_range(NAMESPACE_WELL_KNOWN, "mainPage", 0, 4)
        .expect("get_range through redirect");
    let home_bytes: Vec<u8> = format!("<h1>Home</h1>{}", " word".repeat(500)).into_bytes();
    assert_eq!(total as usize, home_bytes.len());
    assert_eq!(slice, &home_bytes[..4]);
}

#[test]
fn get_range_not_found() {
    let data = build_sample(false);
    let mut r = Reader::from_bytes(data).expect("reader");

    let err = r
        .get_range(NAMESPACE_CONTENT, "nonexistent", 0, 10)
        .expect_err("should not find");
    assert!(err.is_not_found());
}
