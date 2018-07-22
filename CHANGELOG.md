## 0.4.0 (TBD)

* The _client_ module is sigificantly expanded per below. The module and
  feature are renamed _async_ and made a default feature.  A deprecated
  _client_ re-export and feature alias are remain available.

* New `AsyncBodyImage` (symetric with `AsyncBodySink`) implements
 `futures::Stream` and `hyper::body::Payload` traits. The `Payload` trait
 makes this usable with hyper as the `B` body type of `http::Request<B>`
 (client) or `http::Response<B>` (server). The `Stream` trait is
 sufficient for use via `hyper::Body::with_stream`.

* Alternative new `UniBodyImage` and `UniBodySink` implement the same
  traits with a custom `UniBodyBuf` item buffer type, allowing zero-copy
  including when in `MemMap` state, at the the cost of the adjustments
  required for not using the default `hyper::Body` type. (*mmap* feature
  only)

* Replace the last Box<Future> with `Either` to avoid heap allocation.

* Broaden and improve _async_ module tests and catalog by type as _stub_,
  _server_, _futures_, and (non-default, further limited) _live_.

* New benchmarks of `AsyncBodyImage` and `UniBodyImage` stream transfer of
  8MiB bodies, from states `Ram` (also incl. "pregather" as in prior and
  per-iteration `gather`), `FsRead` incl. various read buffer sizes, and
  `MemMap` (also incl. "pre-" as in prior). `AsyncBodyImage` `MemMap`
  performance is the worst ("stream_22_mmap_copy"), as it combines a copy
  with a cache-busting 8MiB buffer size.  Note that `FsRead` and `MemMap`
  also benefit from OS file-system caching as the same file is used
  per-iteration. Results:

   dev i7-2640M, rustc 1.29.0-nightly (e06c87544 2018-07-06):
   ```text
   test stream_01_ram_pregather ... bench:      10,004 ns/iter (+/- 445)
   test stream_02_ram           ... bench:     228,891 ns/iter (+/- 88,360)
   test stream_03_ram_uni       ... bench:     166,857 ns/iter (+/- 6,472)
   test stream_04_ram_gather    ... bench:   4,959,583 ns/iter (+/- 350,443)
   test stream_10_fsread        ... bench:   1,820,107 ns/iter (+/- 561,514)
   test stream_11_fsread_uni    ... bench:   1,908,536 ns/iter (+/- 323,490)
   test stream_12_fsread_8k     ... bench:   2,374,895 ns/iter (+/- 911,142)
   test stream_13_fsread_128k   ... bench:   1,850,793 ns/iter (+/- 544,267)
   test stream_14_fsread_1m     ... bench:   1,927,205 ns/iter (+/- 198,953)
   test stream_15_fsread_4m     ... bench:   5,679,301 ns/iter (+/- 941,365)
   test stream_20_mmap_uni_pre  ... bench:      14,161 ns/iter (+/- 385)
   test stream_21_mmap_uni      ... bench:      29,410 ns/iter (+/- 3,950)
   test stream_22_mmap_copy     ... bench:   8,110,105 ns/iter (+/- 4,213,807)
   ```

## 0.3.0 (2018-6-26)
* Updates to _client_ module for better composability with _tokio_
  applications and fully asynchronous response body support (#2):
  * Upgrade to _hyper_ 0.12.x and _tokio_ (reform, 0.1.x).
  * New `AsyncBodySink` implements `futures::Sink` for `BodySink`,
    supporting fully asynchronous receipt of a `hyper::Body` request
    or response stream.
  * New `request_dialog` function returns `impl Future<Item=Dialog>`
    given a `RequestRecord` and suitable `hyper::Client` reference.
  * Timeout support via _tokio-timer_ and `Tunables::res_timeout` and
    `body_timeout` durations.

* Upgrade (optional default) _brotli_ to >=2.2.1, <3.

* Minimal rustc version upgraded to (and CI tested at) 1.26.2 for use
  of `impl Trait` feature.

* Remove methods that were deprecated in 0.2.0: `*::try_clone`,
  `BodyImage::prepare`. (#3)

* Add thread name/id logging to cli and client integration tests
  (optionally via env TEST_LOG number).

## 0.2.0 (2018-5-8)
* Concurrent `FsRead` readers and infallible `BodyImage::clone`
  support (#1):
  * `BodyReader` uses `ReadPos`, from new _olio_ crate, for the
    `FsRead` state.
  * `BodyImage`, `Dialog` and `Record` now implement infallible
    `Clone::clone`. The `try_clone` methods are deprecated.
  * `BodyImage::prepare` is now no-op, deprecated.

* Memory mapping is now an entirely optional, explicitly called,
  default feature:
  * Previously `try_clone` automatically _upgraded_ clones to `MemMap`,
    and `write_to` created a temporary mapping, both to avoid
    concurrent mutation of `FsRead`. Now `ReadPos` makes this
    unnecessary.
  * The `BarcReader` previously mapped large (per
    `Tunables::max_body_ram`), uncompressed bodies. Now it uses
    `ReadSlice` for concurrent, direct positioned read access in this
    case.

* Add `BodyImage::from_file` (and `from_read_slice`) conversion
  constructors.

## 0.1.0 (2018-4-17)
* Initial release
