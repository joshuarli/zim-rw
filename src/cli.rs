use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;
use zim::{Reader, Writer, NAMESPACE_CONTENT, NAMESPACE_WELL_KNOWN};

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(err) = run() {
        eprintln!("zim: {err}");
        std::process::exit(1);
    }
}

fn run() -> CliResult<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => {
            let (root, out, quiet) = parse_build_args(&args[1..])?;
            let output = build_archive(Path::new(&root), out.as_deref(), quiet)?;
            println!("{}", output.display());
            Ok(())
        }
        Some("serve") if args.len() == 2 => serve_archive(Path::new(&args[1])),
        _ => {
            eprintln!("usage:");
            eprintln!("  zim build <rootdir> [-o|--out <file.zim>] [-q|--quiet]");
            eprintln!("  zim serve <file.zim>");
            std::process::exit(2);
        }
    }
}

fn parse_build_args(args: &[String]) -> CliResult<(String, Option<PathBuf>, bool)> {
    let mut root: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for -o/--out".into());
                }
                out = Some(PathBuf::from(&args[i]));
            }
            "-q" | "--quiet" => quiet = true,
            arg if arg.starts_with('-') => {
                return Err(format!("unknown flag: {arg}").into());
            }
            arg => {
                if root.is_some() {
                    return Err(format!("unexpected argument: {arg}").into());
                }
                root = Some(arg.to_owned());
            }
        }
        i += 1;
    }
    match root {
        Some(root) => Ok((root, out, quiet)),
        None => Err("missing <rootdir> argument".into()),
    }
}

fn build_archive(root: &Path, out: Option<&Path>, quiet: bool) -> CliResult<PathBuf> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()).into());
    }

    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.is_empty() {
        return Err("root directory contains no files".into());
    }

    let mut writer = Writer::new();
    for (url, path) in &files {
        let size = fs::metadata(path)?.len();
        writer.add_entry(NAMESPACE_CONTENT, url, "", mime_for_path(path), size);
    }

    let main_url = files
        .iter()
        .find(|(url, _)| url == "index.html")
        .or_else(|| files.iter().find(|(url, _)| url.ends_with("/index.html")))
        .unwrap_or(&files[0])
        .0
        .clone();
    writer.add_redirect(
        NAMESPACE_WELL_KNOWN,
        "mainPage",
        "mainPage",
        NAMESPACE_CONTENT,
        main_url,
    );
    writer.set_main_page(NAMESPACE_WELL_KNOWN, "mainPage");

    let output = match out {
        Some(path) => path.to_path_buf(),
        None => output_path_for_root(root)?,
    };

    let path_refs: Vec<&PathBuf> = files.iter().map(|(_, p)| p).collect();
    let total = files.len();
    let mut packed = 0usize;
    let show_progress = !quiet && io::stderr().is_terminal();

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output)?;
    writer.write_to_streaming(
        &mut file,
        |idx| {
            let data = fs::read(path_refs[idx])?;
            packed += 1;
            if show_progress {
                progress(packed, total, &files[idx].0);
            }
            Ok(data)
        },
        0,
    )?;
    file.flush()?;

    if show_progress {
        eprint!("\r\x1b[K");
        let _ = io::stderr().flush();
    }
    if !quiet {
        eprintln!("wrote {} files to {}", files.len(), output.display());
    }
    Ok(output)
}

fn progress(n: usize, total: usize, label: &str) {
    let w = 20;
    let filled = if total == 0 { 0 } else { n * w / total };
    let mut bar = String::with_capacity(w + 2);
    bar.push('[');
    for i in 0..w {
        bar.push(if i < filled {
            '='
        } else if i == filled {
            '>'
        } else {
            ' '
        });
    }
    bar.push(']');
    // Trailing spaces ensure we clear any leftover chars from a longer previous line
    eprint!("\r  {bar} {n}/{total} {label}                \r");
    let _ = io::stderr().flush();
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in WalkDir::new(dir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.into_path();
            let rel = path.strip_prefix(root).expect("entry is under root");
            let url = path_to_url(rel);
            if !url.is_empty() {
                out.push((url, path));
            }
        }
    }
    Ok(())
}

fn path_to_url(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn output_path_for_root(root: &Path) -> CliResult<PathBuf> {
    let name = root
        .file_name()
        .ok_or("root directory must have a file name")?
        .to_string_lossy();
    Ok(root.with_file_name(format!("{name}.zim")))
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css",
        "gif" => "image/gif",
        "htm" | "html" => "text/html",
        "jpeg" | "jpg" => "image/jpeg",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "txt" => "text/plain",
        "wasm" => "application/wasm",
        "webp" => "image/webp",
        "xml" => "application/xml",
        _ => "application/octet-stream",
    }
}

fn serve_archive(path: &Path) -> CliResult<()> {
    let addr = env::var("ZIM_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let listener = TcpListener::bind(&addr)?;
    let local_addr = listener.local_addr()?;
    println!("Listening on http://{local_addr}");
    io::stdout().flush()?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream, path) {
                    eprintln!("zim: request failed: {err}");
                }
            }
            Err(err) => eprintln!("zim: accept failed: {err}"),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, zim_path: &Path) -> CliResult<()> {
    let mut first_line = String::new();
    {
        let mut reader = BufReader::new(&stream);
        reader.read_line(&mut first_line)?;
    }

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" && method != "HEAD" {
        write_response(
            &mut stream,
            405,
            "text/plain",
            b"method not allowed",
            method == "HEAD",
        )?;
        return Ok(());
    }

    let mut reader = Reader::open(zim_path)?;
    let result = if target == "/" {
        reader.main_page()
    } else {
        let url = request_target_to_url(target);
        reader.get(NAMESPACE_CONTENT, &url).or_else(|err| {
            if url.ends_with('/') {
                reader.get(NAMESPACE_CONTENT, &format!("{url}index.html"))
            } else if !url.contains('.') {
                reader.get(NAMESPACE_CONTENT, &format!("{url}/index.html"))
            } else {
                Err(err)
            }
        })
    };

    match result {
        Ok(blob) => write_response(
            &mut stream,
            200,
            content_type(&blob.mime_type),
            &blob.data,
            method == "HEAD",
        )?,
        Err(err) if err.is_not_found() => write_response(
            &mut stream,
            404,
            "text/plain",
            b"not found",
            method == "HEAD",
        )?,
        Err(err) => {
            let body = format!("archive error: {err}");
            write_response(
                &mut stream,
                500,
                "text/plain",
                body.as_bytes(),
                method == "HEAD",
            )?
        }
    }
    Ok(())
}

fn request_target_to_url(target: &str) -> String {
    let path = target.split('?').next().unwrap_or(target);
    percent_decode(path.trim_start_matches('/'))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(a << 4 | b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn content_type(mime_type: &str) -> &str {
    if mime_type.is_empty() {
        "application/octet-stream"
    } else {
        mime_type
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}
