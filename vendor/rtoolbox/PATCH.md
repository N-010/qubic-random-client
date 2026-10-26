# Local rtoolbox patch

Source: crates.io rtoolbox 0.0.5, checksum
`50a0e551c1e27e1731aba276dbeaeac73f53c7cd34d1bda485d02bd1e0f36844`.
The upstream Apache-2.0 license is retained in LICENSE-APACHE.

The sole source change is SafeString::drop: use zeroize::Zeroize for String
instead of overwriting only its current length. This also clears spare capacity
containing characters removed with Backspace before an input error. The crate
adds zeroize, which is already used by RandomClient. Public APIs are unchanged.

The root Cargo.toml patches crates.io resolution so rpassword uses this copy.
The regression test tests/terminal_buffer_cleanup.rs observes initialized
synthetic bytes immediately before the allocator frees the buffer. It covers
unchanged, truncated and cleared strings, plus successful ownership transfer.
It fails against the original upstream implementation.

This patch does not promise erasure of allocations previously released during
String growth, copies elsewhere, or cleanup after abrupt process termination.
On an upstream update, retain the patch until the regression passes without it.
