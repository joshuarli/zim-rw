use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;
use zim::{Error, NAMESPACE_CONTENT, NAMESPACE_WELL_KNOWN, Reader, Writer};

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
            let (root, out, quiet, level) = parse_build_args(&args[1..])?;
            let output = build_archive(Path::new(&root), out.as_deref(), quiet, level)?;
            println!("{}", output.display());
            Ok(())
        }
        Some("extract") => {
            let (zim_path, out, quiet) = parse_extract_args(&args[1..])?;
            extract_archive(Path::new(&zim_path), out.as_deref(), quiet)?;
            Ok(())
        }
        Some("serve") if args.len() == 2 => serve_archive(Path::new(&args[1])),
        Some("add-fec") => {
            let (zim_path, redundancy) = parse_add_fec_args(&args[1..])?;
            add_fec(Path::new(&zim_path), redundancy)?;
            Ok(())
        }
        Some("repair") => {
            let zim_path = parse_repair_args(&args[1..])?;
            repair(Path::new(&zim_path))?;
            Ok(())
        }
        _ => {
            eprintln!("usage:");
            eprintln!(
                "  zim build <rootdir> [-o|--out <file.zim>] [-q|--quiet] [-l|--level <1-22>]"
            );
            eprintln!("  zim extract <file.zim> [-o|--out <dir>] [-q|--quiet]");
            eprintln!("  zim serve <file.zim>");
            eprintln!("  zim add-fec <file.zim> [-r|--redundancy <pct>]");
            eprintln!("  zim repair <file.zim>");
            std::process::exit(2);
        }
    }
}

fn parse_build_args(args: &[String]) -> CliResult<(String, Option<PathBuf>, bool, Option<i32>)> {
    let mut root: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut quiet = false;
    let mut level: Option<i32> = None;
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
            "-l" | "--level" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for -l/--level".into());
                }
                level = Some(
                    args[i]
                        .parse()
                        .map_err(|_| format!("invalid compression level: {}", args[i]))?,
                );
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
        Some(root) => Ok((root, out, quiet, level)),
        None => Err("missing <rootdir> argument".into()),
    }
}

fn build_archive(
    root: &Path,
    out: Option<&Path>,
    quiet: bool,
    level: Option<i32>,
) -> CliResult<PathBuf> {
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
    if let Some(l) = level {
        writer.set_compression_level(l);
    }
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
    let filled = (n * w).checked_div(total).unwrap_or(0);
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

fn parse_extract_args(args: &[String]) -> CliResult<(String, Option<PathBuf>, bool)> {
    let mut zim: Option<String> = None;
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
                if zim.is_some() {
                    return Err(format!("unexpected argument: {arg}").into());
                }
                zim = Some(arg.to_owned());
            }
        }
        i += 1;
    }
    match zim {
        Some(zim) => Ok((zim, out, quiet)),
        None => Err("missing <file.zim> argument".into()),
    }
}

fn extract_archive(zim_path: &Path, out: Option<&Path>, quiet: bool) -> CliResult<()> {
    if !zim_path.is_file() {
        return Err(format!("{} is not a file", zim_path.display()).into());
    }

    let out_dir = match out {
        Some(path) => path.to_path_buf(),
        None => {
            let stem = zim_path
                .file_stem()
                .ok_or("zim file must have a file name")?
                .to_string_lossy();
            zim_path.with_file_name(stem.as_ref())
        }
    };
    fs::create_dir_all(&out_dir)?;

    let mut reader = Reader::open(zim_path)?;
    let count = reader.count();
    let mut extracted = 0usize;
    let show_progress = !quiet && io::stderr().is_terminal();

    for idx in 0..count {
        let entry = reader.entry_at(idx)?;
        if entry.redirect || entry.namespace != NAMESPACE_CONTENT {
            continue;
        }
        let file_path = out_dir.join(&entry.url);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file_path, &entry.data)?;
        extracted += 1;
        if show_progress {
            progress(extracted, extracted, &entry.url);
        }
    }

    if show_progress {
        eprint!("\r\x1b[K");
        let _ = io::stderr().flush();
    }
    if !quiet {
        eprintln!("extracted {} files to {}", extracted, out_dir.display());
    }
    Ok(())
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
    let (method, target, range) = {
        let mut buf = BufReader::new(&stream);
        let mut first_line = String::new();
        buf.read_line(&mut first_line)?;

        let mut parts = first_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let target = parts.next().unwrap_or_default().to_owned();

        // Read headers to find Range requests.
        let mut range: Option<(u64, u64)> = None;
        loop {
            let mut line = String::new();
            buf.read_line(&mut line)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(val) = trimmed
                .to_ascii_lowercase()
                .strip_prefix("range:")
                .map(|v| v.trim().to_owned())
            {
                range = parse_range(&val);
            }
        }
        (method, target, range)
    };

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

    // For range requests, use get_range to fetch only the requested bytes.
    // For uncompressed clusters this slices the mmap'd data directly without
    // decompressing the entire cluster.
    if let Some((start, end)) = range {
        let range_len = end.saturating_sub(start).saturating_add(1);
        let result = try_get_url(&mut reader, &target, |r, ns, url| {
            r.get_range(ns, url, start, range_len)
        });
        match result {
            Ok((data, total)) => {
                let ct = mime_for_target(&target);
                write_response_range(&mut stream, ct, &data, total, start, end, method == "HEAD")?;
            }
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
                )?;
            }
        }
        return Ok(());
    }

    let result = try_get_url(&mut reader, &target, |r, ns, url| {
        let blob = r.get(ns, url)?;
        Ok((blob.data, blob.mime_type))
    });

    match result {
        Ok((data, mime_type)) => {
            write_response_bytes(
                &mut stream,
                200,
                content_type(&mime_type),
                &data,
                method == "HEAD",
            )?;
        }
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

/// Parse a single `bytes=N-M` range.  Returns (start, end) inclusive, clamped
/// to the content length later.
fn parse_range(val: &str) -> Option<(u64, u64)> {
    let val = val.strip_prefix("bytes=")?;
    let (start_str, end_str) = val.split_once('-')?;
    if start_str.is_empty() {
        // suffix range: bytes=-N (last N bytes) — not supported for simplicity
        return None;
    }
    let start: u64 = start_str.parse().ok()?;
    if end_str.is_empty() {
        // open-ended: bytes=N- (from N to end) — not supported for simplicity
        return None;
    }
    let end: u64 = end_str.parse().ok()?;
    if start > end {
        return None;
    }
    Some((start, end))
}

/// Resolves an HTTP request target to a (namespace, url) and calls `f` with
/// the result.  For `/` the main page is used.  Other paths try the URL
/// directly, then `/index.html` or `index.html` fallbacks.
fn try_get_url<T>(
    reader: &mut Reader,
    target: &str,
    f: impl Fn(&mut Reader, u8, &str) -> Result<T, Error>,
) -> Result<T, Error> {
    if target == "/" {
        let (ns, url) = reader.main_page_ref()?.ok_or(Error::NotFound {
            namespace: 0,
            url: String::new(),
        })?;
        return f(reader, ns, &url);
    }
    let url = request_target_to_url(target);
    f(reader, NAMESPACE_CONTENT, &url).or_else(|err| {
        if url.ends_with('/') {
            f(reader, NAMESPACE_CONTENT, &format!("{url}index.html"))
        } else if !url.contains('.') {
            f(reader, NAMESPACE_CONTENT, &format!("{url}/index.html"))
        } else {
            Err(err)
        }
    })
}

/// Returns a MIME type for an HTTP request target based on its extension.
fn mime_for_target(target: &str) -> &'static str {
    let url = target.rsplit('/').next().unwrap_or(target);
    let ext = url.rsplit('.').next().unwrap_or("");
    match ext {
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

fn request_target_to_url(target: &str) -> String {
    let path = target.split('?').next().unwrap_or(target);
    percent_decode(path.trim_start_matches('/'))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(a << 4 | b);
            i += 3;
            continue;
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
    let reason = status_reason(status);
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

/// Serves a blob with `Accept-Ranges: bytes` so browsers can request partial
/// content for efficient loading of large files.
fn write_response_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    let reason = status_reason(status);
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn write_response_range(
    stream: &mut TcpStream,
    content_type: &str,
    body: &[u8],
    total: u64,
    start: u64,
    end: u64,
    head_only: bool,
) -> io::Result<()> {
    // Clamp to valid range
    let end = end.min(total.saturating_sub(1));
    if start >= total || start > end {
        write!(
            stream,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nConnection: close\r\n\r\n"
        )?;
        return stream.flush();
    }
    write!(
        stream,
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nContent-Range: bytes {start}-{end}/{total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn parse_add_fec_args(args: &[String]) -> CliResult<(String, u32)> {
    let mut zim: Option<String> = None;
    let mut redundancy: u32 = 25;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-r" | "--redundancy" => {
                i += 1;
                if i >= args.len() {
                    return Err("missing value for -r/--redundancy".into());
                }
                redundancy = args[i]
                    .parse()
                    .map_err(|_| format!("invalid redundancy percentage: {}", args[i]))?;
                if redundancy == 0 || redundancy > 100 {
                    return Err("redundancy must be between 1 and 100".into());
                }
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown flag: {arg}").into());
            }
            arg => {
                if zim.is_some() {
                    return Err(format!("unexpected argument: {arg}").into());
                }
                zim = Some(arg.to_owned());
            }
        }
        i += 1;
    }
    match zim {
        Some(zim) => Ok((zim, redundancy)),
        None => Err("missing <file.zim> argument".into()),
    }
}

fn parse_repair_args(args: &[String]) -> CliResult<String> {
    let mut zim: Option<String> = None;
    for arg in args {
        if arg.starts_with('-') {
            return Err(format!("unknown flag: {arg}").into());
        }
        if zim.is_some() {
            return Err(format!("unexpected argument: {arg}").into());
        }
        zim = Some(arg.to_owned());
    }
    match zim {
        Some(zim) => Ok(zim),
        None => Err("missing <file.zim> argument".into()),
    }
}

fn add_fec(zim_path: &Path, redundancy: u32) -> CliResult<()> {
    if !zim_path.is_file() {
        return Err(format!("{} is not a file", zim_path.display()).into());
    }
    let name = zim_path
        .file_name()
        .ok_or("zim file must have a file name")?
        .to_string_lossy()
        .into_owned();
    let dir = zim_path
        .parent()
        .ok_or("zim file must have a parent directory")?;

    // Run par2create in the file's directory, then tar up the PAR2 files and
    // append them after the ZIM checksum.  This is forwards-compatible: existing
    // readers ignore trailing data.
    let status = Command::new("par2create")
        .args([format!("-r{redundancy}"), "-n1".to_string(), name.clone()])
        .current_dir(dir)
        .status()
        .map_err(|err| format!("failed to run par2create: {err}"))?;
    if !status.success() {
        return Err("par2create failed".into());
    }

    let par2_name = format!("{name}.par2");
    let par2_path = dir.join(&par2_name);
    if !par2_path.is_file() {
        return Err(format!("par2create did not produce {}", par2_path.display()).into());
    }

    // Collect PAR2 volume files by finding all .par2 files matching the pattern
    let mut par2_files: Vec<PathBuf> = vec![par2_path];
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ename = entry.file_name().to_string_lossy().into_owned();
        if ename.starts_with(&format!("{name}.vol")) && ename.ends_with(".par2") {
            par2_files.push(entry.path());
        }
    }

    // Append tar of PAR2 files to the ZIM
    let mut tar_args: Vec<String> = par2_files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    tar_args.insert(0, "fc".to_owned());
    tar_args.insert(1, "-".to_owned());

    let tar = Command::new("tar")
        .args(&tar_args)
        .current_dir(dir)
        .output()
        .map_err(|err| format!("failed to run tar: {err}"))?;
    if !tar.status.success() {
        return Err(format!("tar failed: {}", String::from_utf8_lossy(&tar.stderr)).into());
    }

    let mut file = OpenOptions::new().append(true).open(zim_path)?;
    file.write_all(&tar.stdout)?;
    file.flush()?;

    // Clean up the standalone PAR2 files
    for p in &par2_files {
        let _ = fs::remove_file(p);
    }

    let tar_size = tar.stdout.len();
    println!(
        "appended {tar_size} byte{} of PAR2 FEC ({redundancy}% redundancy) to {}",
        if tar_size == 1 { "" } else { "s" },
        zim_path.display()
    );
    Ok(())
}

fn repair(zim_path: &Path) -> CliResult<()> {
    if !zim_path.is_file() {
        return Err(format!("{} is not a file", zim_path.display()).into());
    }
    let name = zim_path
        .file_name()
        .ok_or("zim file must have a file name")?
        .to_string_lossy()
        .into_owned();
    let dir = zim_path
        .parent()
        .ok_or("zim file must have a parent directory")?;

    // par2repair needs a .par2 extension to recognise the file type.  We create
    // a symlink, point par2repair at the symlink, and pass the real file as an
    // extra file to scan for embedded PAR2 packets.
    let symlink_name = format!("{name}.par2");
    let symlink_path = dir.join(&symlink_name);
    let _ = fs::remove_file(&symlink_path);
    std::os::unix::fs::symlink(&name, &symlink_path)
        .map_err(|err| format!("failed to create symlink for repair: {err}"))?;

    let output = Command::new("par2repair")
        .args([symlink_name, name.clone()])
        .current_dir(dir)
        .output()
        .map_err(|err| format!("failed to run par2repair: {err}"))?;

    // Clean up regardless of outcome
    let _ = fs::remove_file(&symlink_path);

    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        return Err("par2repair failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_basic() {
        assert_eq!(parse_range("bytes=0-99"), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-199"), Some((100, 199)));
        assert_eq!(parse_range("bytes=0-0"), Some((0, 0)));
    }

    #[test]
    fn parse_range_rejects_suffix() {
        assert_eq!(parse_range("bytes=-500"), None);
    }

    #[test]
    fn parse_range_rejects_open_ended() {
        assert_eq!(parse_range("bytes=100-"), None);
    }

    #[test]
    fn parse_range_rejects_descending() {
        assert_eq!(parse_range("bytes=100-50"), None);
    }

    #[test]
    fn parse_range_rejects_invalid() {
        assert_eq!(parse_range("bytes=abc-def"), None);
        assert_eq!(parse_range(""), None);
        assert_eq!(parse_range("garbage"), None);
    }

    #[test]
    fn parse_range_with_whitespace() {
        // The caller strips the "Range:" header name, lowercases, and trims;
        // parse_range receives the pre-trimmed value.
        assert_eq!(parse_range("bytes=0-99"), Some((0, 99)));
        assert_eq!(parse_range(" bytes=50-100"), None); // not trimmed here
    }
}
