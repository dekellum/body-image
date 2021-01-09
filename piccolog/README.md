# piccolog

[![Rustdoc](https://docs.rs/piccolog/badge.svg)](https://docs.rs/piccolog)
[![Change Log](https://img.shields.io/crates/v/piccolog.svg?maxAge=3600&label=change%20log&color=9cf)](https://github.com/dekellum/body-image/blob/master/piccolog/CHANGELOG.md)
[![Crates.io](https://img.shields.io/crates/v/piccolog.svg?maxAge=3600)](https://crates.io/crates/piccolog)
[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=master)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

A very minimal `Log` output implementation for testing of body-image*, barc*
and related crates, and for use with `barc-cli`. Output is directed to
STDERR. When logging with tests, a `TEST_LOG` environment variable is read to
configure a course-grained logging level:

`TEST_LOG=0`
: The default, no logging enabled.

`TEST_LOG=1`
: The `Info` log level.

`TEST_LOG=2`
: The `Debug` log level, but dependencies are filtered to `Info` log level.

`TEST_LOG=3`
: The `Debug` log level (for all).

`TEST_LOG=4` (or higher)
: The `Trace` log level (for all).

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
for inclusion in piccolog by you, as defined by the Apache License, shall be
dual licensed as above, without any additional terms or conditions.
