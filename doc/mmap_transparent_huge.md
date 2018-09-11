Since linux kernel 4.7, tmpfs supports huge pages. Using `huge` mount
option set to `always` or (untested) `within_size` will achieve
*transparent* huge pages, without any mmap or madvise flags required.

In the future this may be supported by other file systems such as ext4
(see reading below).

Setup instructions for testing:

   ```bash
   sudo mkdir /mnt/htmp
   # where 16M is sufficient for the benchmark and uid/gid=1000 is the
   # bench test user
   sudo mount -t tmpfs -o size=16M,huge=always,uid=1000,gid=1000 tmpfs /mnt/htmp
   ```

See also the TMPFS(5) man page. Then via `cargo bench mmap_uni`, with
changes to `benches/async_streams.rs` on this branch:

   dev i7-5600U, rustc 1.30.0-nightly (d5a448b3f 2018-08-13)
   ```text
   test stream_20_mmap_uni_pre   ... bench:      24,773 ns/iter (+/- 4,234)
   test stream_210_mmap_uni_htmp ... bench:      82,660 ns/iter (+/- 39,001)
   test stream_211_mmap_uni_tmp  ... bench:     711,365 ns/iter (+/- 179,446)
   test stream_21_mmap_uni       ... bench:     707,204 ns/iter (+/- 184,151)
   ```

Where *mmap_uni_htmp* is setup to use /mnt/htmp (above tmpfs) and
*mmap_uni_tmp* is control with normal (default page size) /tmp
(default tmpfs). Only for the brief time while *mmap_uni_htmp* is
running, /proc/meminfo shows the expected usage of huge pages:

   ```text
   ShmemHugePages:     8192 kB
   ```

Monitor via:

   ```bash
   while true; do grep ShmemHugePages /proc/meminfo && sleep 0.05; done
   ```

Further reading:

* https://www.kernel.org/doc/Documentation/vm/transhuge.txt

* [Huge pages in ext4 (lwn)](https://lwn.net/Articles/718102/)
