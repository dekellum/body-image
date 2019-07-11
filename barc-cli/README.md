# barc-cli

[![Crates.io](https://img.shields.io/crates/v/barc-cli.svg?maxAge=3600)](https://crates.io/crates/barc-cli)
[![Travis CI Build](https://travis-ci.org/dekellum/body-image.svg?branch=master)](https://travis-ci.org/dekellum/body-image)
[![Appveyor CI Build](https://ci.appveyor.com/api/projects/status/0c2e9x4inktasxgf/branch/master?svg=true)](https://ci.appveyor.com/project/dekellum/body-image)

A command line tool for printing, recording, de-/compressing and copying BARC
records (format defined by the *barc* library crate).

## Minimum supported rust version

MSRV := 1.31.0

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
