# barc-cli

[![Change Log](https://img.shields.io/crates/v/barc-cli.svg?maxAge=3600&label=change%20log&color=9cf)](https://github.com/dekellum/body-image/blob/main/barc-cli/CHANGELOG.md)
[![Crates.io](https://img.shields.io/crates/v/barc-cli.svg?maxAge=3600)](https://crates.io/crates/barc-cli)
[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=main)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

A command line tool for printing, recording, de-/compressing and copying BARC
records (format defined by the *barc* library crate).

## Minimum supported rust version

MSRV := 1.45.2

If none of the default features _futio_, _brotli_, or _mmap_ are enabled, then
the MSRV remains 1.39.0.

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
