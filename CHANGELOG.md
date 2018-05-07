## 0.2.0 (TBD)
* Concurrent `FsRead` readers and infallible `BodyImage::clone`
  support (#1):
  * `BodyReader` uses new `ReadPos` for the `FsRead` state. `ReadPos`
     re-implements `Read` and `Seek` over a shared `File` using _only_
     positioned reads and an instance-specific position.
  * `BodyImage`, `Dialog` and `Record` now implement infallible
    `Clone::clone`. The `try_clone` methods are deprecated.
  * `BodyImage::prepare` is now no-op, deprecated

* Memory mapping is now an entirely optional, explicitly called,
  feature:
  * Previously `try_clone` automaticly _upgraded_ clones to `MemMap`,
    and `write_to` created a temporary mapping, both to avoid
    concurrent mutation of `FsRead`. Now `ReadPos` makes this
    unnecessary.
  * The `BarcReader` previously mapped large (per
    `Tunables::max_body_ram`), uncompressed bodies, now it uses the
    new `ReadSlice` for concurrent, direct positioned read access in
    this case.

* Add benchmarks of reads from `GatheringReader`, chained
  `std::io::Cursor` and upfront `BodyImage::gather` with single
  `Cursor`.

  On my dev host; i7-5600U, rustc 1.27.0-nightly (bd40cbbe1 2018-04-14):
  ``` text
  test gather_chained_cursors   ... bench:     558,877 ns/iter (+/- 90,532)
  test gather_reader            ... bench:      63,256 ns/iter (+/- 2,294)
  test gather_upfront           ... bench:      64,078 ns/iter (+/- 14,701)
  test gather_upfront_read_only ... bench:      40,490 ns/iter (+/- 3,578)
  ```

## 0.1.0 (2018-4-17)
* Initial release
