# barc

[![Rustdoc](https://docs.rs/barc/badge.svg)](https://docs.rs/barc)
[![Change Log](https://img.shields.io/crates/v/barc.svg?maxAge=3600&label=change%20log&color=9cf)](https://github.com/dekellum/body-image/blob/main/barc/CHANGELOG.md)
[![Crates.io](https://img.shields.io/crates/v/barc.svg?maxAge=3600)](https://crates.io/crates/barc)
[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=main)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

The **B**ody **Arc**hive (BARC) container file format, reader and
writer. Supports high fidelity serialization of complete HTTP
request/response dialogs with additional meta-data and has broad use
cases as test fixtures or for caching or web crawling.  A `barc`
command line tool is also available, via the *barc-cli* crate.

See the rustdoc for more details.

## Minimum supported rust version

MSRV := 1.46.0

The crate will fail fast on any lower rustc (via a build.rs version check) and
is also CI tested on this version. MSRV will only be increased in a new MINOR
(or MAJOR) release of this crate. However, some direct or transitive
dependencies unfortunately have or may increase MSRV in PATCH releases. Users
may need to selectively control updates by preserving/distributing a Cargo.lock
file in order to control MSRV.

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
