# body-image-futio

[![Rustdoc](https://docs.rs/body-image-futio/badge.svg)](https://docs.rs/body-image-futio)
[![Change Log](https://img.shields.io/crates/v/body-image-futio.svg?maxAge=3600&label=change%20log&color=9cf)](https://github.com/dekellum/body-image/blob/main/body-image-futio/CHANGELOG.md)
[![Crates.io](https://img.shields.io/crates/v/body-image-futio.svg?maxAge=3600)](https://crates.io/crates/body-image-futio)
[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=main)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

The _body-image-futio_ crate integrates _body-image_ with _futures_,
_http_, _hyper_, and _tokio_ for both client and server use.

## Minimum supported rust version

MSRV := 1.45.2

The crate will fail fast on any lower rustc (via a build.rs version check) and
is also CI tested on this version. MSRV will only be increased in a new MINOR
(or MAJOR) release of this crate. However, some direct or transitive
dependencies unfortunately have or may increase MSRV in PATCH releases. Known
examples are listed below:

* http 0.2.5 increased MSRV to 1.46.0
* hyper 0.14.5 adds a socket2 0.4 dependency, MSRV 1.46.0

Users may need to selectively control updates by preserving/distributing a
Cargo.lock file in order to control MSRV.

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
