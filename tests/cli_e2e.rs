use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
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
