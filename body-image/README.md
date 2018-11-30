# body-image

[![Crates.io](https://img.shields.io/crates/v/body-image.svg?maxAge=3600)](https://crates.io/crates/body-image)
[![Rustdoc](https://docs.rs/body-image/badge.svg)](https://docs.rs/body-image)
[![Travis CI Build](https://travis-ci.org/dekellum/body-image.svg?branch=master)](https://travis-ci.org/dekellum/body-image)
[![Appveyor CI Build](https://ci.appveyor.com/api/projects/status/0c2e9x4inktasxgf/branch/master?svg=true)](https://ci.appveyor.com/project/dekellum/body-image)

The _body-image_ crate provides a uniform access strategy for HTTP body
payloads which may be scattered across multiple allocations in RAM, or buffered
to a temporary file, and optionally memory mapped. This effectively enables
trading some file I/O cost in return for supporting significantly larger bodies
without risk of exhausting RAM.

See the top-level (project workspace) README for additional rationale.

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
