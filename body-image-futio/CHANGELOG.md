## 2.3.0 (unreleased)
* Minimum supported rust version (MSRV) is now 1.46.0.

* Upgrade to body-image 2.3.

* Broaden _httparse_ dependency to include 1.5.

* Broaden _hyperx_ dependency to include 1.4.

* Broaden _tokio_ dependency to include 1.13.

* Replace `impl Into<Bytes> for UniBodyBuf` with
  `impl From<UniBodyBuf> for Bytes`.

## 2.2.2 (2021-10-28)
* Broaden _bytes_ dependency to include 1.1.

* Broaden _httparse_ dependency to include 1.4.

* Broaden _tokio_ dependency to include 1.2-1.12 releases.

* Fix `cfg` gates for `InDialog` struct.

* Clarify MSRV policy and list problem updates.

## 2.2.1 (2021-1-23)
* Broaden _tokio_ dependency to include new 1.1.z releases.

* Update dev. dependency _rand_ to 0.8.0.

* Minor documentation improvements.

* Add clippy config for primordial MSRV build.rs and for current MSRV.

## 2.2.0 (2021-1-18)
* Upgrade to body-image 2.2 (MSRV 1.39.0)

* Upgrade to bytes 1.0, http-body 0.4.0, and lift constraint on http dependency
  (MSRV 1.39.0).

* Upgrade to hyperx 1.3 (MSRV 1.39.0)

* Upgrade to olio 1.4 (MSRV 1.39.0).

* Upgrade to hyper 0.14, hyper-tls 0.4 (MSRV 1.45.2)

* Upgrade to tokio 1.0 (MSRV 1.45.2)

* Upgrade to blocking-permit 1.3.0 (MSRV 1.45.2)

* Minimum supported rust version is now 1.45.2 (per above upgrades).

## 2.1.2 (2021-1-17)
* Upgrade to hyperx 1.2.1.

* Constrain http to <0.2.3 to avoid bytes duplicates.

* Misc documentation improvements.

## 2.1.1 (2020-2-5)
* Remove selection of tokio's _parking_lot_ feature erroneously introduced in
  2.1.0. This conflicts with our tokio 0.2.6 minimum (where that feature
  wasn't available). More generally, we should allow applications to opt-in to
  such features, rather than mandate what isn't strictly required here.

## 2.1.0 (2020-2-4 _yanked, with 2.1.1 as replacement_)
* Add `blocking-permit::Splittable` implementation for `UniBodyBuf`.

* Add `SplitBodyImage` and `YieldBodyImage` wrapper types.

* Upgrade to blocking-permit 1.2.0 for cleaver and yield-stream features.

* Replace fern logger (dev dependency) with piccolog.

## 2.0.2 (2020-1-16)
* Correct and clarify why there is, actually, no practical risk of out-of-order
  reads/writes with the `DispatchBodyImage` and `DisptachBodySink` types,
  respectively. The 2.0.1 release overstated this risk.

* Restore benchmarks erroneously removed in 2.0.1 for multiple-dispatch pool
  threads.

## 2.0.1 (2020-1-15)
* Fix blocking I/O interrupt handling for `AsyncBodySink`. Fix logging of
  interrupt for `AsyncBodyStream`.

* Extend blocking-permit dependency to include 1.1.0, for its improved
  run-on-caller ordering.  Add warnings to `DispatchBodyImage` and
  `DisptachBodySink` regarding the risk of out-of-order reads/writes if the
  pool is configured with more than one thread.

* Additional benchmarks with use of bounded
  `blocking-permit::DispatchPoolBuilder::queue_length` and preference to one
  dispatch thread, to avoid out-of-order writes.

## 2.0.0 (2020-1-13)
* `AsyncBodyImage` and `AsyncBodySink` are now generic over `Item` type
  (e.g. `Bytes` or `UniBodyBuf`). The prior `UniBodyImage` and `UniBodySink`
  types are removed (replaced with use of `UniBodyBuf` generic parameter).

* Introduce wrapper types `DispatchBodyImage`, `PermitBodyImage`,
  `DispatchBodySink`, `PermitBodySink` with additional coordination when
  blocking operations (e.g. filesystem reads and writes) are required.

* Introduce `StreamWrapper` and `SinkWrapper` traits for generic use of all
  above listed `Stream` and `Sink` types.

* Introduce `FutioTunables` composing, and `FutioTuner` extending, body-image
  `Tunables` and `Tuner` respectively; with options moved/added and used here:
  `res_timeout`, `body_timeout`, `blocking_policy`. As this `FutioTunables` can
  now be cloned without allocation, most interfaces using it are now
  pass-by-value (move).

* Upgrade to body-image 2.0.0 including interface changes
  (e.g. `push`, `Tunables`).

* Upgrade to http 0.2.0 and bytes 0.5.2 (MSRV 1.39.0)

* Upgrade to olio 1.3.0 (MSRV 1.34.0)

* Upgrade to tao-log 1.0.0 (MSRV 1.32.0)

* Update to flate2 [1.0.4, 1.1), dropping the constraint introduced in 1.3.0
  (MSRV 1.34.0)

* Upgrade to hyper 0.13.1, hyper-tls 0.4.0
  (MSRV 1.39.0, now optional behind hyper-http feature gate)

* Upgrade to hyperx 1.0.0 (MSRV 1.39.0)

* Upgrade to futures-* 0.3.1 (using minimal sub-crates)

* Upgrade to tokio 0.2.6 (MSRV 1.39.0, using minimal features)

* Add hyper-http feature to gate hyper dependency.

* Add brotli-decompressor >= 2.1.2 min transitive upgrade

* Minimum supported rust version is now 1.39.0 (per above upgrades).

## 1.3.0 (2019-10-1)
* Fix build.rs for `rustc --version` not including git metadata.

* Upgrade to body-image 1.3.0 and olio 1.2.0, including tempfile 3.1.0 and
  rand 0.7.

* Upgrade tokio minimums to most recent tokio 0.1.22, tokio-reactor 0.1.10,
  tokio-threadpool 0.1.16 for parity with above and to avoid large numbers of
  duplicates.

* Minimum supported rust version is now 1.32.0 (to match above upgrades).

* Narrow flate2 dependency to < 1.0.10, to avoid MSRV breakage. flate2
  1.0.10 and 1.0.11 releases have MSRV 1.34.0 (alexcrichton/flate2-rs#207)

## 1.2.0 (2019-5-13)
* Upgrade to hyperx 0.15.0 and use new `TypedHeaders` extension to simplify
  `find_chunked` and to improve content-length handling, which will now error
  by design on the edge case of multiple, non-equal length values.

* Narrow various dependencies to avoid future MINOR versions, for reliability.
  We may subsequently make PATCH releases which _broaden_ private or public
  dependencies to include new MINOR releases found _compatible_. Rationale:
  Cargo fails to consider MSRV for dependency resolution (rust-lang/rfcs#2495),
  and MSRV bumps are common in MINOR releases, including as planned here.

* Increase minimum brotli version to 3.1.0, based on minimal version testing
  with latest tests.

* Add build.rs script to fail fast on an attempt to compile with a rustc below
  MSRV, which remains 1.31.0.

## 1.1.1 (2019-5-8)
* Previously functions `find_encodings` and `find_chunked` were sensitive to
  the order of transfer/content-encoding header value lists, and while the
  ordering is under-specified, had assumed the opposite of documented examples
  in order of _application_, e.g. "gzip, chunked". With this reasonably common
  case, these functions would fail to find `Encoding::Chunked`, since they
  terminated at the first recognized compression encoding.  Fix this by not
  terminating the scan early. Also add support for parsing "x-gzip" as
  alternate for `Encoding::Gzip`, per HTTP 1.1. Finally, handle the edge case
  of multiple compression encodings by using the _last_ (per application order,
  and parsing transfer-encoding before content-encoding) and logging a warning
  of the additional encodings found.

## 1.1.0 (2019-3-6)
* _Error reform_: add `FutioError` enum and remove _failure_ crate dependency:
  * Introduce new `FutioError` enum for the most common error cases. This type
    implements `StdError`, aka `std::error::Error`.
  * Replace `failure::Error`, used for encapsulation of otherwise private
    dependency errors, with compatible
    `Box<StdError + Send + Sync + 'static>`, type-aliased as `Flaw` and in
    `FutioError::Other(Flaw)`.
  * Add `FutioError::UnsupportedEncoding` for use by `decompress` and
    `decode_res_body` for unsupported encodings.  Previously this case was
    treated as if no compression encoding was found.
  * `RequestRecorder<T>` trait methods now simply return `http::Error` for the
    `Err(E)` type. None of the implementations have any other errors and are
    unlikely to have them in the future. Struct `http::Error` is also
    `StdError`, and for convenience and compatiblity, a `From` conversion to a
    `FutioError::Http(http::Error)` variant is included.

  Since `failure::Fail` offers a blanket implementation for `StdError`, which
  includes `Flaw` and `FutioError`, this is graded a MINOR-version
  compatibility hazard. Testing of unmodified dependent crates/projects
  supports this conclusion.

* Function `decode_res_body` now appends `Encoding::Identity` to the
  `res_decoded` list upon success, regardless of if a compression encoding was
  found to decode. This can be used an an explicit indicator that the check
  was made and the associated content-type *should* characterize the current
  `BodyImage`. For a use case, see `barc::CompressStrategy::check_identity` as
  used by the latest _barc-cli_ to avoid double, non-productive compression.

* Upgrade to body-image 1.1.0, for use of `BodyReader` as direct `Read`
  implementation.

* Broaden (optional default feature) _brotli_ dependency to >=2.2.1, <4.

* Improve log and test output via _tao-log_ crate macros.

## 1.0.3 (2019-1-11)
* Upgrade to tokio 0.1.14 and require only the tokio feature flags (and
  sub-crates) that are used. In combination with the latest hyper 0.12.20, this
  avoids unused dependencies: mio-uds, tokio-codec, tokio-fs, tokio-udp,
  tokio-uds.

* Upgrade to tokio-threadpool 0.1.10 for minimal compatibility with the above.

## 1.0.2 (2019-1-4)
* Fix missed upgrade to hyperx 0.14 in prior release.

## 1.0.1 (2019-1-4 _yanked_)
* Upgrade to hyperx 0.14.0 and use new support for faster direct `HeaderValue`
  parsing.

* Simplify find_encodings function.

* Upgrade to hyper-tls 0.3.0 to avoid old minimal versions.

* Upgrade log and lazy_static deps to reflect 2018 minimal versions.

## 1.0.0 (2018-12-4)
* Update to the rust 2018 edition, including the changes to pass all 2018 idiom
  lints (anchored paths, anonymous/elided lifetimes).  _This start of the 1.x
  release series has a minimum supported rust version of 1.31.0, and is thus
  potentially less stable than prior 0.x releases, which will continue to be
  maintained as needed._

* Separate into its own *body-image-futio* crate (see prior history below). As
  of the 2018 edition *async* is a reserved keyword, so substitute *futio* for
  crate name and remaining optional feature and dependency of the *barc-cli*
  crate.

* Upgrade (dev dep) to rand 0.6.1, fix deprecation in benchmark.

* Use this crate's VERSION for default `user_agent`.

## History in *body-image*

Previously *body-image-futio* was released as the *async* module and feature of
the *body-image* crate.  Relevent release history is extracted below:

## body-image 0.5.0 (2018-10-19)
* Drop `async::RequestRecordable` type alias for the `RequestRecorder<B>`
  trait, which was intended to offer backward compatibility (to 0.3.0), but
  wasn't usable. Type aliases to traits can be *defined* but not really *used*
  in rust to date.

* Replace use of "deadline" with "timeout", per the deprecation of the former in
  tokio-rs/tokio#558, which was released as tokio 0.1.8 (and tokio-timer 0.2.6)
  patch releases.

* Replace use of `tokio::thread_pool::Builder` with the new
  `tokio::runtime::Builder` façade methods. The former was deprecated and the
  latter added in tokio-rs/tokio#645. This was originally released in tokio
  0.1.9. Subsequently tokio 0.1.10 and 0.1.11 were released as (purely semver)
  bug fixes of the former, so make 0.1.11 the new minimum version.

* Use `dyn Trait` syntax throughout. Minimum supported rust version is now
  1.27.2.

## body-image 0.4.0 (2018-8-15)
* The _client_ module and feature are renamed _async_, made a default feature
  and significantly expanded. See the _async_ module subsection below for
  further details. A deprecated _client_ re-export and feature alias remain
  available.

* All internal `Mmap` (*mmap* feature) reads have been optimized using the
  concurrent-aware `olio::mem::MemHandle::advise` for `Sequential` access where
  appropriate. As of _olio_ 0.4.0, this is limited to \*nix platforms via
  `libc::posix_madvise`.

* Remove dependency on the *failure_derive* crate, and disable the _derive_
  feature of the *failure* crate dependency.

* New `AsyncBodyImage` (symmetric with `AsyncBodySink`) implements
 `futures::Stream` and `hyper::body::Payload` traits. The `Payload` trait
 makes this usable with hyper as the `B` body type of `http::Request<B>`
 (client) or `http::Response<B>` (server). The `Stream` trait is sufficient
 for use via `hyper::Body::with_stream`.

* Alternative new `UniBodyImage` and `UniBodySink` implement the same
  traits with a custom `UniBodyBuf` item buffer type, allowing zero-copy
  including when in `MemMap` state, at the the cost of the adjustments
  required for not using the default `hyper::Body` type. (*mmap* feature
  only)

* Both `Tunables` timeouts are now optional, with the initial
  (res)ponse timeout now defaulting to `None`. Thus by default only
  the full body timeout is used in `request_dialog`.

* Replace the only remaining use of `Box<Future>` with `Either` to avoid
  heap allocation.

* Broaden and improve _async_ module tests and catalog by type as _stub_,
  _server_, _futures_, and (non-default, further limited) _live_.

* New benchmarks of `AsyncBodyImage` and `UniBodyImage` stream transfer of
  8MiB bodies, from states `Ram` (also incl. "pregather", as in prior, and
  per-iteration `gather`), `FsRead` incl. various read buffer sizes, and
  `MemMap` (also incl. "pre-" as in prior). `AsyncBodyImage` `MemMap`
  performance is the worst ("stream_22_mmap_copy"), as it combines a copy
  with a cache-busting 8MiB buffer size.  Note that `FsRead` and `MemMap`
  states also benefit from OS file-system caching, as the same file is used
  per-iteration. Results:

   dev i7-2640M, rustc 1.29.0-nightly (e06c87544 2018-07-06):
   ```text
   test stream_01_ram_pregather ... bench:      21,719 ns/iter (+/- 4,840)
   test stream_02_ram           ... bench:     284,504 ns/iter (+/- 63,189)
   test stream_03_ram_uni       ... bench:     270,463 ns/iter (+/- 67,336)
   test stream_04_ram_gather    ... bench:   3,088,980 ns/iter (+/- 496,452)
   test stream_10_fsread        ... bench:   1,933,098 ns/iter (+/- 470,207)
   test stream_11_fsread_uni    ... bench:   1,921,628 ns/iter (+/- 397,760)
   test stream_12_fsread_8k     ... bench:   2,526,199 ns/iter (+/- 502,436)
   test stream_13_fsread_128k   ... bench:   2,008,152 ns/iter (+/- 428,687)
   test stream_14_fsread_1m     ... bench:   2,048,519 ns/iter (+/- 439,723)
   test stream_15_fsread_4m     ... bench:   3,260,075 ns/iter (+/- 916,301)
   test stream_20_mmap_uni_pre  ... bench:      42,342 ns/iter (+/- 19,692)
   test stream_21_mmap_uni      ... bench:     626,874 ns/iter (+/- 144,886)
   test stream_22_mmap_copy     ... bench:   3,780,882 ns/iter (+/- 901,138)
   ```

   dev i7-5600U, rustc 1.29.0-nightly (e06c87544 2018-07-06)
   ```text
   test stream_01_ram_pregather ... bench:      19,376 ns/iter (+/- 3,609)
   test stream_02_ram           ... bench:     214,196 ns/iter (+/- 63,079)
   test stream_03_ram_uni       ... bench:     202,167 ns/iter (+/- 42,734)
   test stream_04_ram_gather    ... bench:   4,321,976 ns/iter (+/- 696,701)
   test stream_10_fsread        ... bench:   1,575,203 ns/iter (+/- 570,972)
   test stream_11_fsread_uni    ... bench:   1,397,239 ns/iter (+/- 535,468)
   test stream_12_fsread_8k     ... bench:   2,035,047 ns/iter (+/- 675,939)
   test stream_13_fsread_128k   ... bench:   1,616,827 ns/iter (+/- 363,059)
   test stream_14_fsread_1m     ... bench:   1,478,323 ns/iter (+/- 499,075)
   test stream_15_fsread_4m     ... bench:   5,289,406 ns/iter (+/- 1,426,670)
   test stream_20_mmap_uni_pre  ... bench:      21,514 ns/iter (+/- 4,513)
   test stream_21_mmap_uni      ... bench:     685,744 ns/iter (+/- 165,161)
   test stream_22_mmap_copy     ... bench:   5,861,194 ns/iter (+/- 1,454,946)
   ```

## body-image 0.3.0 (2018-6-26)
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

* Add thread name/id logging to cli and client integration tests
  (optionally via env TEST_LOG number).

## body-image 0.2.0 (2018-5-8)
* Minor change to use `Dialog::clone` (infallible clone support, #1)

## body-image 0.1.0 (2018-4-17)
* Initial release
