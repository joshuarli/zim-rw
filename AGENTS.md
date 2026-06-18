# Project Notes

This crate is a deliberately small ZIM reader/writer. It is not a Rust port of
libzim's full API. Keep changes scoped to the subset below unless the user asks
to expand it.

# Implemented ZIM Subset

- Header: 80-byte ZIM header, little-endian fields, magic `0x044d495a`, and
  major versions 5 or 6 accepted when reading.
- Namespace scheme: the modern single-content namespace layout.
  - `C` for content pages/assets.
  - `M` for metadata.
  - `W` for well-known redirects such as `W/mainPage`.
- Directory entries:
  - Normal item entries.
  - Redirect entries using MIME sentinel `0xffff`.
  - UTF-8 paths and titles are preserved as bytes/string data.
- Clusters:
  - Stored/uncompressed clusters.
  - Zstd clusters.
  - Old compression flag `0` is treated as stored/uncompressed when reading.
  - Extended 64-bit cluster offsets are readable.
  - The writer currently emits non-extended 32-bit offset clusters.
- Writer behavior:
  - Deterministic output for identical input.
  - URL pointer table sorted by `<namespace><url>`.
  - Title pointer table sorted by `<namespace><title>`, then URL index.
  - Text-like MIME types are zstd-compressed unless `set_no_compress(true)` is
    used.
  - Binary-like MIME types are stored uncompressed.
  - A trailing MD5 checksum is written.
- Reader behavior:
  - Random lookup by namespace and URL.
  - Redirects are followed by `get()` and `main_page()`.
  - `entry_at()` exposes stored entries without following redirects.
  - Decompressed clusters are cached.
  - `check()` verifies the trailing MD5 checksum on demand.

# Explicit Non-Goals

Do not add these without a specific user request:

- Xapian search indexes or suggestion indexes.
- ICU or language analysis.
- LZMA/XZ, zip, or bzip2 support.
- Split/multipart ZIM files.
- Full libzim archive APIs.
- Full old namespace-scheme compatibility.
- Writer-generated libzim metadata extras such as `X/listing/titleOrdered/v1`,
  `M/Counter`, automatic illustrations, aliases, or front-article hints.
- Network access, clocks, global mutable runtime state, or background workers.

# Architecture

- `src/lib.rs`
  Public crate surface: re-exports, `Result`, and `Error`.
- `src/format.rs`
  Wire constants, header marshal/parse helpers, little-endian helpers, and MD5.
  Keep binary-format details centralized here.
- `src/codec.rs`
  Zstd encode/decode and MIME classification for deciding whether writer
  clusters should be compressed.
- `src/reader.rs`
  `Reader`, `Blob`, and `Entry`. Handles header/MIME parsing, binary lookup,
  dirent parsing, redirect resolution, cluster decompression, cluster caching,
  extended offsets, and checksum verification.
- `src/writer.rs`
  `Writer` and planning/serialization internals. Builds MIME lists, pointer
  tables, dirents, clusters, deterministic UUIDs, and checksum trailers.
- `tests/roundtrip.rs`
  Ported and reference-inspired behavior tests. Add new format compatibility
  tests here before broadening implementation behavior.


# Development Rules

- Keep dependencies minimal. Current direct dependencies are `zstd`, `memchr`,
  `md5`, and `walkdir` for the demo CLI. Current dev-dependency is `tempfile`.
- Do not add banner/separator comments.
- Preserve useful comments when refactoring.
- Run `cargo fmt`, `cargo test`, and `cargo clippy --all-targets -- -D warnings`
  before handing off substantial changes.
- Do not run pre-commit hooks.
- Do not push to a remote.
