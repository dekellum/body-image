# body-image

[![Rustdoc](https://docs.rs/body-image/badge.svg)](https://docs.rs/body-image)
[![Change Log](https://img.shields.io/crates/v/body-image.svg?maxAge=3600&label=change%20log&color=9cf)](https://github.com/dekellum/body-image/blob/master/body-image/CHANGELOG.md)
[![Crates.io](https://img.shields.io/crates/v/body-image.svg?maxAge=3600)](https://crates.io/crates/body-image)
[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=master)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

The _body-image_ crate provides a uniform access strategy for HTTP body
payloads which may be scattered across multiple allocations in RAM, or buffered
to a temporary file, and optionally memory mapped. This effectively enables
trading some file I/O cost in return for supporting significantly larger bodies
without risk of exhausting RAM.

See the top-level (project workspace) README for additional rationale.

## Minimum supported rust version

MSRV := 1.39.0

The crate will fail fast on any lower rustc (via a build.rs version
check) and is also CI tested on this version.

## License

This project is dual licensed under either of following:

* The Apache License, version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)
  or http://www.apache.org/licenses/LICENSE-2.0)

* The MIT License ([LICENSE-MIT](LICENSE-MIT)
  or http://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in body-image by you, as defined by the Apache License, shall be
dual licensed as above, without any additional terms or conditions.
