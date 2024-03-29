# The Body Image Project

[![CI Status](https://github.com/dekellum/body-image/workflows/CI/badge.svg?branch=main)](https://github.com/dekellum/body-image/actions?query=workflow%3ACI)

A rust language project providing separately usable but closely related crates:

The namesake _[body-image]_ crate provides a uniform access strategy for HTTP body
payloads which may be scattered across multiple allocations in RAM, or buffered
to a temporary file, and optionally memory mapped. This effectively enables
trading some file I/O cost in return for supporting significantly larger bodies
without risk of exhausting RAM.

The _[body-image-futio]_ crate integrates the _body-image_ crate with
_futures_, _http_, _hyper_, and _tokio_ for both client and server use.

The _[barc]_ crate provides the **B**ody **Arc**hive (BARC) container file
format, reader and writer. This supports high fidelity and human readable
serialization of complete HTTP request/response dialogs with additional
meta-data and has broad use cases as test fixtures or for caching or web
crawling.

The _[barc-cli]_ crate provides a command line tool for printing, recording,
de-/compressing, and copying BARC records.

See the above rustdoc links or the README(s) and CHANGELOG(s) under the
individual crate directories for more details.

[body-image]: https://docs.rs/crate/body-image
[barc]: https://docs.rs/crate/barc
[barc-cli]: https://crates.io/crates/barc-cli
[body-image-futio]: https://docs.rs/crate/body-image-futio

## Rationale

HTTP sets no limits on request or response body payload sizes, and in general
purpose libraries or services, we are reluctant to enforce the low maximum
size constraints necessary to *guarantee* sufficient RAM and reliable
software. This is exacerbated by all of the following:

* The concurrent processing potential afforded by both threads and Rust's
  asynchronous facilities: Divide the available RAM by the maximum number of
  request/response bodies in memory at any one point in time.

* With chunked transfer encoding, we frequently don't know the size of the
  body until it is fully downloaded (no Content-Length header).

* Transfer or Content-Encoding compression: Even if the compressed body fits
  in memory, the decompressed version may not, and its final size is not known
  in advance.

* Constrained memory: Virtual machines and containers tend to have less RAM
  than our development environments, as do mobile devices. Swap space is
  frequently not configured, or if used, results in poor performance.

Note there are different opinions on this topic, and implementations. For
example, HAProxy which is a RAM-only proxy by design, recently [introduced a
"small object cache"][HAProxy] limited by default to 16 KiB complete
responses. Nginx by comparison offers a hybrid RAM and disk design. When
buffering proxied responses, by current defaults on x86_64 it will keep 64 KiB
in RAM before buffering to disk, where the response is finally limited to 1
GiB.

This author thinks the operational trends toward denser virtual allocation
instead of growth in per-instance RAM, in combination with increasing
availability of fast solid state disk (e.g. NVMe SSDs) make hybrid approaches
more favorable to more applications than was the case in the recent past.

[HAProxy]: https://www.haproxy.com/blog/whats-new-haproxy-1-8/

## License

This project is dual licensed under either of following:

* The Apache License, version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)
  or http://www.apache.org/licenses/LICENSE-2.0)

* The MIT License ([LICENSE-MIT](LICENSE-MIT)
  or http://opensource.org/licenses/MIT)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the body image project by you, as defined by the Apache
License, shall be dual licensed as above, without any additional terms or
conditions.
