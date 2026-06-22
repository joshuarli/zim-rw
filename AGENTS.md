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
  - `X` for generated listings such as `X/listing/titleOrdered/v1`.
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
  - Cluster packing prioritises HTML first, then CSS, then by URL depth so
    the first cluster contains the most important content.
- Writer behavior:
  - Deterministic output for identical input.
  - URL pointer table sorted by `<namespace><url>`.
  - Title pointer table sorted by `<namespace><title>`, then URL index.
  - Text-like MIME types are zstd-compressed unless `set_no_compress(true)` is
    used.
  - Binary-like MIME types are stored uncompressed.
  - A trailing MD5 checksum is written.
  - Auto-generates `M/Counter` and `X/listing/titleOrdered/v1`.
- Reader behavior:
  - Memory-mapped I/O (mmap) for zero-copy access to uncompressed data.
  - Random lookup by namespace and URL.
  - Title-based lookup via `get_by_title` and `entries_by_title_prefix`.
  - Redirects are followed by `get()` and `main_page()`.
  - `entry_at()` exposes stored entries without following redirects.
  - Sub-blob partial reads via `get_range()`: arbitrary byte ranges from a
    blob, sliced directly from the mmap'd backing for uncompressed clusters
    (no decompression), or from cached decompressed data for compressed
    clusters.
  - Decompressed clusters are cached with a bounded LRU-like eviction
    (default 64 entries; configurable via `set_cache_limit`).
- CLI (`zim`):
  - `zim build <rootdir>` — build a ZIM from a directory tree.
  - `zim extract <file.zim>` — extract content entries to a directory.
  - `zim serve <file.zim>` — HTTP server with Range request support
    (`206 Partial Content`, `Accept-Ranges: bytes`) for efficient
    loading of large files.
  - `zim add-fec <file.zim> [-r <pct>]` — append PAR2 forward error
    correction after the checksum for bit-rot recovery.
  - `zim repair <file.zim>` — repair a corrupted ZIM using embedded
    PAR2 FEC data.

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
  dirent parsing, redirect resolution, cluster decompression, bounded cluster
  caching, extended offsets, and checksum verification.
- `src/writer.rs`
  `Writer` and planning/serialization internals. Builds MIME lists, pointer
  tables, dirents, clusters with priority packing (HTML before CSS before
  depth-ordered assets), deterministic UUIDs, and checksum trailers.
- `src/cli.rs`
  CLI binary: `build`, `extract`, `serve` (with HTTP Range support),
  `add-fec` (PAR2 FEC), and `repair`.
- `tests/roundtrip.rs`
  Ported and reference-inspired behavior tests. Add new format compatibility
  tests here before broadening implementation behavior.
- `tests/cli_e2e.rs`
  End-to-end CLI tests including build-extract round-trips, HTTP serving with
  Range requests, and asset ordering verification.

# libzim Reference

When looking for patterns, the C++ reference implementation at `../libzim`
provides useful prior art. Key architectural points:

- **mmap**: We use `memmap2` for zero-copy access to uncompressed data,
  matching libzim's `mmap` with `MAP_POPULATE`.
- **Cluster cache**: libzim's `ConcurrentCache` is a 16 MiB LRU with
  `shared_future`-based miss dedup (two threads requesting the same cluster
  share one decompression).  Our cache is simpler (generation-based, no
  concurrency) but bounded.
- **Sub-blob access**: Our `Reader::get_range()` mirrors libzim's
  `Cluster::getBlob(n, offset, size)`, extracting arbitrary byte ranges
  from a blob. For uncompressed clusters the range is sliced directly from
  the mmap'd backing without decompression.
- **No HTTP server in libzim**: All serving logic lives in `kiwix-serve` or
  similar consumers.  libzim provides the data-access primitives only.


# Development Rules

- Keep dependencies minimal. Current direct dependencies are `zstd`, `memchr`,
  `md5`, and `walkdir` for the demo CLI. Current dev-dependency is `tempfile`.
- Do not add banner/separator comments.
- Preserve useful comments when refactoring.
- Run `cargo fmt`, `cargo test`, and `cargo clippy --all-targets -- -D warnings`
  before handing off substantial changes.
- Do not run pre-commit hooks.
- Do not push to a remote.
