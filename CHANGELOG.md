## 0.2.0 (TBD)
* Concurrent `FsRead` and infallible `BodyImage::clone` support (#1):
  * No mandatory memory mapping. Previously `try_clone` _upgraded_ to
    `MemMap`, and `write_to` created a temporary mapping, to avoid
    (concurrent) mutation of `FsRead`.
  * `BodyReader` uses new `ReadPos` for the `FsRead` state.  `ReadPos`
    re-implements `Read` and `Seek` over a shared `File` using _only_
    positioned reads and an instance-specific position.
  * `BodyImage`, `Dialog` and `Record` now implement infallible
    `Clone::clone`. The `try_clone` methods are deprecated.
  * `BodyImage::prepare` is now no-op, deprecated
  * Add benchmarks of GatheringReader vs chained Cursors. On my dev host:

     test gather_chained_cursors ... bench:     551,646 ns/iter (+/- 68,241)
     test gather_reader          ... bench:      62,994 ns/iter (+/- 7,839)
     test gather_upfront         ... bench:      65,717 ns/iter (+/- 81,202)

## 0.1.0 (2018-4-17)
* Initial release
