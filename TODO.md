# TODO

This list is for modern, narrow-scope improvements. Do not use it as permission
to grow the crate toward full libzim parity or legacy compatibility.

# High-Value Modern Extras

## Title-Based Lookup APIs

Add title-based entry lookup using the title pointer table we already generate.

Suggested reader APIs:

- `get_by_title(&mut self, namespace: u8, title: &str) -> Result<Blob>`
- `entries_by_title_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>>`

Implementation notes:

- Use the title pointer table (sorted by title) for O(log n) lookup.
- Include redirects as stored entries.
- Add tests for exact match, no match, duplicate titles, namespace filtering.
- This is low-hanging fruit — the pointer table is already written correctly.

## `X/listing/titleOrdered/v1` Generation

Generate a title-ordered listing entry for compatibility with modern
libzim/Kiwix readers.

Notes:

- This is a generated blob in the `X` namespace whose content lists all
  entries in title order. Study libzim's format before implementing.
- Required by Kiwix-serve for title browsing.
- Add tests that verify determinism and correct ordering.

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

## MIME Counter Metadata

Add writer support for `M/Counter`.

Expected behavior:

- Count non-redirect content entries by MIME type.
- Write a text metadata entry named `Counter` with `mime=count` lines.
- Keep output deterministic by sorting MIME keys lexicographically.
- Add tests for one MIME, multiple types, redirects excluded, deterministic output.

## Illustration Helpers

Add a helper for archive icons/illustrations.

Suggested API shape:

- `add_illustration(&mut self, width: u32, height: u32, scale: u32, png: impl Into<Vec<u8>>)`

Expected behavior:

- Write metadata key `Illustration_{width}x{height}@{scale}` with MIME `image/png`.
- Re-adding the same key replaces the prior bytes.

## Writer Validation

Add explicit validation before planning/writing:

- NUL bytes in URL, title, MIME, or metadata names.
- Redirect targets that do not exist.
- Main page target that does not exist.
- Non-extended writer clusters that would exceed `u32::MAX` offset capacity.
- Empty URL where it would produce ambiguous lookup behavior.

## Prefix Lookup APIs

Add prefix lookup in the Reader:

- `entries_by_path_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>>`
- `entries_by_title_prefix(&mut self, namespace: u8, prefix: &str) -> Result<Vec<Entry>>`

## Public API Polish

- Audit method names, argument order, and return types.
- Decide whether byte namespaces should become a `Namespace` type.
- Add rustdoc examples for basic build/read/check flows.
- Add a compile-only doc test that exercises only public APIs.

# Second-Tier Improvements

## Alias or Cloned Blob Support

Consider an API for multiple URLs sharing one blob without duplicating data.

- `add_alias(namespace, url, title, target_namespace, target_url)`
- Should create a normal item entry pointing at the target's cluster/blob, not a redirect.

## Content Deduplication (Content-Addressed Blobs)

The ZIM format already allows multiple dirents to point at the same
cluster + blob.  The writer could hash blobs and reuse identical ones from
the same cluster, which would transparently deduplicate assets shared
across different URL paths (same JS/CSS/image included under multiple
names).

- Hash each incoming blob (SHA-256 or similar).
- Maintain a `HashMap<Hash, (cluster, blob)>` during cluster packing.
- If a blob's hash matches a previously written blob in the same cluster,
  point the dirent at the existing cluster/blob instead of writing a copy.
- This stays within the ZIM format — readers need no changes.
- Inspired by Gwtar's note that content-addressed naming enables
  deduplication across archives.

## Extended Cluster Offsets for Large Archives

When a cluster's uncompressed size exceeds `u32::MAX`, automatically emit
extended (64-bit) cluster offsets. Currently we only read them.

## Real Fixture Compatibility

Add compatibility tests against real modern ZIM fixtures:

- Use small, checked-in fixtures if licensing and size allow.
- Verify opening, checksum, main page, metadata, redirects, and content blobs.

## Cross-Implementation Checks

Exercise interoperability with the C++ reference implementation:

- Add ignored or opt-in tests that build with this crate and read with libzim.
- Keep these out of normal `cargo test` unless external projects are configured.

## Fuzzing and Parser Robustness

Add fuzz/property coverage for binary parsing:

- Header, MIME list, directory entry, cluster offset table parsing.
- Assert malformed inputs return errors, not panics.

## Documentation Quality

- Expand crate-level docs with supported subset and non-goals.
- Add examples for creating an archive, reading a blob, iterating entries, verifying checksum.
- Link this TODO from the README.

# Non-Goals

Do not implement these without a separate user request:

- LZMA/XZ, zip, or bzip2 support.
- Split/multipart ZIM files.
- Old namespace-scheme compatibility.
- Xapian search, suggestions, or indexing.
- ICU or language-aware lookup.
- Full libzim `Archive` API parity.
- Network access, clocks, global mutable runtime state, or background workers.
