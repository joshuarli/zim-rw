# TODO

This list is for modern, narrow-scope improvements. Do not use it as permission
to grow the crate toward full libzim parity or legacy compatibility.

# High-Value Modern Extras

## Typed Metadata Helpers

Add convenience APIs on `Writer` for common metadata entries while keeping the
existing generic `add_metadata` and `add_metadata_bytes` methods.

Suggested API shape:

- `set_title(&mut self, value: impl Into<String>)`
- `set_language(&mut self, value: impl Into<String>)`
- `set_description(&mut self, value: impl Into<String>)`
- `set_creator(&mut self, value: impl Into<String>)`
- `set_publisher(&mut self, value: impl Into<String>)`
- `set_date(&mut self, value: impl Into<String>)`

Implementation notes:

- These should write `M/Title`, `M/Language`, `M/Description`, `M/Creator`,
  `M/Publisher`, and `M/Date`.
- Use `text/plain` unless there is a strong reason to standardize on a more
  specific MIME type.
- Keep behavior deterministic and replacement-friendly.
- Add tests proving the helpers create readable metadata entries and replace
  prior values for the same metadata key.

## MIME Counter Metadata

Add writer support for `M/Counter`.

Expected behavior:

- Count non-redirect content entries by MIME type.
- Write a text metadata entry named `Counter`.
- Format should match libzim/Kiwix convention: one `mime=count` pair per line,
  for example `text/html=123`.
- Decide and document whether metadata entries themselves are counted. Prefer
  the simplest useful behavior: count user-added content entries, not generated
  metadata.

Implementation notes:

- Do not require users to manually add `M/Counter`.
- Keep output deterministic by sorting MIME keys lexicographically before
  serializing the counter.
- Add tests for one MIME, multiple MIME types, redirects not being counted, and
  deterministic output.

## Illustration Helpers

Add a helper for archive icons/illustrations.

Suggested API shape:

- `add_illustration(&mut self, width: u32, height: u32, scale: u32, png: impl Into<Vec<u8>>)`

Expected behavior:

- Write metadata key `Illustration_{width}x{height}@{scale}`.
- Use MIME type `image/png`.
- Preserve existing `add_metadata_bytes` for non-PNG or custom metadata assets.

Tests:

- `add_illustration(48, 48, 1, bytes)` creates `M/Illustration_48x48@1`.
- Data and MIME type round-trip exactly.
- Re-adding the same illustration key replaces the prior bytes.

## Main Page Redirect Helper

Make conventional `W/mainPage` output easy.

Suggested API shape:

- `set_main_page_redirect(&mut self, target_namespace: u8, target_url: &str)`

Expected behavior:

- Set the header main page to `W/mainPage`.
- Add or replace a redirect entry at `W/mainPage`.
- Redirect target must exist by write time.

Tests:

- `main_page()` resolves to the target content.
- `entry_at()` exposes `W/mainPage` as a redirect.
- Missing target returns a write error.

## Writer Validation

Add explicit validation before planning/writing.

Validation should catch:

- NUL bytes in URL, title, MIME, or metadata names.
- Redirect targets that do not exist.
- Main page target that does not exist.
- Non-extended writer clusters that would exceed `u32::MAX` offset capacity.
- Empty URL where it would produce ambiguous lookup behavior.

Implementation notes:

- Prefer returning `Error` variants over panics or generic strings.
- Keep validation centralized so `write_to` has one clear preflight path.
- Add malformed-input tests for each validation error.

## Public API Polish

Review the public library surface from the perspective of a real caller.

Tasks:

- Audit method names, argument order, and return types for `Reader`, `Writer`,
  `Blob`, and `Entry`.
- Decide whether byte namespaces should stay as `u8` or become a small
  `Namespace` type with constants/conversions.
- Make error variants stable and specific enough for callers to match on.
- Add examples to rustdoc for basic build/read/check flows.
- Decide whether `Reader<File>::open` and `Reader<R>::new` are the right split,
  or whether a path/opening helper should be top-level.
- Add a small compile-only doc test or integration test that uses only public
  APIs.

## Prefix Lookup APIs

Add simple sorted-prefix lookup without search indexes.

Suggested reader APIs:

- `entries_by_path_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>>`
- `entries_by_title_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>>`

Implementation notes:

- Path lookup can use the URL pointer table order.
- Title lookup should use the title pointer table.
- Do not add ICU, stemming, Xapian, tokenization, ranking, or language-specific
  behavior.
- Decide whether prefix lookup should include redirects. Prefer including stored
  entries exactly as `entry_at()` would, so callers can inspect redirects.
- Add tests for no matches, exact match, multiple matches, duplicate titles, and
  namespace filtering.

# Second-Tier Improvements

## Modern Title Listing Entry

Consider generating `X/listing/titleOrdered/v1`.

Notes:

- This is useful for compatibility with modern libzim/Kiwix readers.
- It is an internal generated entry and should not be added before the basic
  metadata/counter/illustration helpers are complete.
- Study libzim behavior before implementing.
- Add tests that verify the entry path, MIME type, blob data, and deterministic
  ordering.

## Alias or Cloned Blob Support

Consider an API for multiple URLs sharing one blob.

Potential API shape:

- `add_alias(namespace, url, title, target_namespace, target_url)`

Notes:

- This should create a normal item entry pointing at the target's cluster/blob,
  not a redirect.
- It can reduce archive size but complicates writer planning.
- Add tests proving aliases do not duplicate blob data and still read as normal
  content entries.

## Configurable Cluster Policy

Expose simple writer tuning knobs.

Potential API shape:

- `set_cluster_size(&mut self, bytes: usize)`
- Optional per-entry compression hint only if there is a concrete caller need.

Notes:

- Keep defaults identical to current behavior.
- Validate cluster size is nonzero and within practical bounds.
- Add tests that small cluster sizes produce multiple readable clusters.

## Streaming Writer Inputs

Consider a provider/streaming input API for large archives.

Notes:

- This changes writer architecture more than the other tasks.
- Do this only after the memory-backed writer is stable.
- Preserve deterministic output.
- Avoid background workers unless there is a proven need.

## Streaming Archive Output

Avoid building the entire archive body in memory during `Writer::write_to`.

Current issue:

- The writer builds a complete `Vec<u8>` body, hashes it, then writes it. This
  is simple but not suitable for large archives.

Tasks:

- Keep the existing two-phase planning model.
- Stream sections to the output while incrementally updating MD5.
- Write the trailing checksum after the streamed body.
- Preserve the returned byte count.
- Add a regression test that compares streamed output byte-for-byte with the
  current deterministic output expectations.

## Demo CLI Hardening

The `zim` binary is a demo exerciser, not a production server. Improve it only
where it helps exercise the library.

Tasks:

- Add `--output <file.zim>` to `zim build`.
- Add `--addr <host:port>` to `zim serve` instead of relying only on
  `ZIM_ADDR`.
- Add graceful shutdown support for tests if it stays simple.
- Keep HTTP parsing minimal, but return clear status codes for malformed
  requests.
- Add E2E coverage for nested `index.html` lookup and percent-decoded paths.

# Quality Hardening

## Real Fixture Compatibility

Add compatibility tests against real modern ZIM fixtures.

Tasks:

- Use small, checked-in fixtures if licensing and size allow.
- Otherwise document a script or ignored local fixture path for opt-in tests.
- Verify opening, checksum, main page, metadata, redirects, and several content
  blobs.
- Compare behavior against libzim for the same fixture where practical.
- Keep legacy fixtures out of default tests unless this crate explicitly expands
  scope.

## Cross-Implementation Checks

Exercise interoperability with the Go and C++ reference implementations without
making them required build dependencies.

Tasks:

- Add ignored or opt-in tests that:
  - Build with this crate and read with libzim or the Go implementation.
  - Build with the Go implementation and read with this crate.
  - Confirm main page, metadata, MIME types, redirects, and content bytes.
- Keep these tests out of the normal `cargo test` path unless the external
  projects are explicitly configured.

## Fuzzing and Parser Robustness

Add fuzz/property coverage for binary parsing.

Targets:

- Header parsing.
- MIME list parsing.
- Directory entry parsing.
- Cluster offset table parsing.
- Redirect resolution.

Expected properties:

- Malformed inputs return errors, not panics.
- Out-of-range offsets are rejected.
- Redirect loops are bounded.
- Extended and non-extended cluster offsets behave consistently.

## Large Archive Behavior

Test behavior near practical and format limits.

Tasks:

- Verify cluster splitting with many files.
- Verify writer errors before a non-extended cluster would exceed `u32::MAX`.
- Add tests for many MIME types and many directory entries.
- Measure memory behavior for writer and reader on moderately large synthetic
  archives.

## Documentation Quality

Document what the crate is and is not.

Tasks:

- Expand crate-level docs with supported subset and non-goals.
- Add examples for:
  - Creating an archive.
  - Reading a blob.
  - Iterating stored entries.
  - Verifying checksum.
- Link `AGENTS.md` and this TODO from the README once a README exists.

# Non-Goals

Do not implement these from this TODO without a separate user request:

- LZMA/XZ, zip, or bzip2 support.
- Split/multipart ZIM files.
- Old namespace-scheme compatibility.
- Xapian search, suggestions, or indexing.
- ICU or language-aware lookup.
- Full libzim `Archive` API parity.
- Network access, clocks, global mutable runtime state, or background workers.
