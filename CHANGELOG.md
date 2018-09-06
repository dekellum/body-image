## 0.5.0 (TBD)

* Mark `BodyImage::from_file` and `BodyImage::from_read_slice` as *unsafe* when
  the default *mmap* feature is enabled, since once `BodyImage::mem_map` is
  called, any concurrent write to the file or other file system modifications
  may lead to *Undefined Behavior*. This is a breaking change as compared with
  0.4.0, though it could also be considered a fix to a regression introduced in
  0.3.0 (#5).

* Replace use of "deadline" with "timeout" per tokio-rs/tokio#558. This was
  released as tokio 0.1.8 (and tokio-timer 0.2.6) patch releases, which
  therefore become our new minimum.

* Re-export the *olio* crate, public dependency (a low probability breaking
  change.)

## 0.4.0 (2018-8-15)

* The _client_ module and feature are renamed _async_, made a default feature
  and significantly expanded. See the _async_ module subsection below for
  further details. A deprecated _client_ re-export and feature alias remain
  available.

* New `BodyImage::explode` returning an `ExplodedImage` enum for raw access to
  individual states.

* All internal `Mmap` (*mmap* feature) reads have been optimized using the
  concurrent-aware `olio::mem::MemHandle::advise` for `Sequential` access where
  appropriate. As of _olio_ 0.4.0, this is limited to \*nix platforms via
  `libc::posix_madvise`.  This feature comes with compatibility breakage:
  * `BodyImage` and therefore `Dialog` are no-longer `Sync`. They remain
    `Send` with inexpensive `Clone`, so any required changes should be
    minimal.

* Deprecate the `BodyReader::File(ReadPos)` variant, instead using
  `BodyReader::FileSlice(ReadSlice)` for this case. This variant is an
  additional complexity with no performance (see _olio_ crate benchmarks)
  or other usage advantage.

* Remove dependency on the *failure_derive* crate, and disable the _derive_
  feature of the *failure* crate dependency, by removing all existing use of
  auto-derive `Fail`.  `BodyError` now has manual implementations of `Display`
  and `Fail`.

### _async_ module

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
