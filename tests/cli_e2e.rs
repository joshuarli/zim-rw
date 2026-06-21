use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn build_then_serve_round_trips_uncompressed_data() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("site");
    fs::create_dir_all(root.join("assets")).expect("create site");

    let index = b"<html><body>Hello from ZIM</body></html>\n".to_vec();
    let asset = vec![0, 1, 2, 3, 4, 5, 255];
    fs::write(root.join("index.html"), &index).expect("write index");
    fs::write(root.join("assets/data.bin"), &asset).expect("write asset");

    let bin = env!("CARGO_BIN_EXE_zim");
    let build = Command::new(bin)
        .arg("build")
        .arg(&root)
        .output()
        .expect("run build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let zim_path = root.with_file_name("site.zim");
    assert!(zim_path.exists());

    let mut server = Command::new(bin)
        .arg("serve")
        .arg(&zim_path)
        .env("ZIM_ADDR", "127.0.0.1:0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn server");

    let stdout = server.stdout.take().expect("server stdout");
    let mut lines = BufReader::new(stdout).lines();
    let line = lines
        .next()
        .expect("server should print listening line")
        .expect("read listening line");
    let addr = line
        .strip_prefix("Listening on http://")
        .expect("listening prefix")
        .to_owned();

    let body = http_get(&addr, "/");
    assert_eq!(body, index);

    let body = http_get(&addr, "/assets/data.bin");
    assert_eq!(body, asset);

    server.kill().expect("kill server");
    server.wait().expect("wait server");
}

#[test]
fn build_extract_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("site");
    fs::create_dir_all(root.join("assets/sub")).expect("create dirs");

    let files: Vec<(&str, &[u8])> = vec![
        ("index.html", b"<html><body>Hello from ZIM</body></html>\n"),
        ("style.css", b"h1 { color: red; }\n"),
        ("assets/logo.png", &[0x89, b'P', b'N', b'G', 0, 1, 2, 3]),
        ("assets/sub/app.js", b"console.log(1);\n"),
        ("readme.txt", b"plain text\n"),
    ];
    for (rel, data) in &files {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, data).expect("write file");
    }

    let bin = env!("CARGO_BIN_EXE_zim");
    let zim_path = root.with_file_name("site.zim");

    let build = Command::new(bin)
        .arg("build")
        .arg(&root)
        .arg("-o")
        .arg(&zim_path)
        .output()
        .expect("run build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let extracted = temp.path().join("out");
    let ext = Command::new(bin)
        .arg("extract")
        .arg(&zim_path)
        .arg("-o")
        .arg(&extracted)
        .output()
        .expect("run extract");
    assert!(
        ext.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&ext.stderr)
    );

    for (rel, expected) in &files {
        let path = extracted.join(rel);
        let actual = fs::read(&path).unwrap_or_else(|_| panic!("missing extracted file: {rel}"));
        assert_eq!(actual, *expected, "mismatch in {rel}");
    }

    // Verify no extra files were extracted
    let mut extracted_files = Vec::new();
    collect_files(&extracted, &extracted, &mut extracted_files).expect("collect");
    assert_eq!(
        extracted_files.len(),
        files.len(),
        "unexpected file count in extracted dir"
    );
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.into_path();
            let rel = path.strip_prefix(root).expect("entry is under root");
            let url = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((url, path));
        }
    }
    Ok(())
}

fn http_get(addr: &str, path: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("headers");
    let headers = String::from_utf8_lossy(&response[..header_end]);
    assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
    response[header_end + 4..].to_vec()
}
