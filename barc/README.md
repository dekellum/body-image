# barc

[![Crates.io](https://img.shields.io/crates/v/barc.svg?maxAge=3600)](https://crates.io/crates/barc)
[![Rustdoc](https://docs.rs/barc/badge.svg)](https://docs.rs/barc)
[![Travis CI Build](https://travis-ci.org/dekellum/body-image.svg?branch=master)](https://travis-ci.org/dekellum/body-image)
[![Appveyor CI Build](https://ci.appveyor.com/api/projects/status/0c2e9x4inktasxgf/branch/master?svg=true)](https://ci.appveyor.com/project/dekellum/body-image)

The **B**ody **Arc**hive (BARC) container file format, reader and
writer. Supports high fidelity serialization of complete HTTP
request/response dialogs with additional meta-data and has broad use
cases as test fixtures or for caching or web crawling.  A `barc`
command line tool is also available, via the *barc-cli* crate.

See the rustdoc for more details.

## Minimum supported rust version

MSRV := 1.34.0

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
