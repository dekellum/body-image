## BodyImage

HTTP sets no limits on request or response body payload sizes, and in
general purpose libraries or services, we are reluctant to enforce the
potentially low maximum size constraints necessary to *guarantee*
sufficient RAM and reliable software. This is exacerbated by all of the
following:

* The concurrent processing potential afforded by both threads and Rust's
  asynchronous facilities: Divide the available RAM by the maximum number
  of request/response bodies in memory at any one point in time.

* With chunked transfer encoding, we frequently don't know the size of the
  body until it is fully downloaded (no Content-Length header).

* Transfer or Content-Encoding compression: Even if the compressed body
  fits in memory, the decompressed version may not, and in most cases we
  don't even know the final size in advance.

* Constrained memory: Virtual hosts and containers tend to have less RAM
  than our development environments, as do mobile devices. Swap is
  frequently not even configured, or if used, results in poor performance.

`BodySink` and `BodyImage` provide logical buffers of bytes which may
not be RAM resident, or may be scattered (discontinuous) in RAM across
separate allocations. `BodySink` is used for accumulating (writing) a
body, and may start or later transition to a temporary file based on
size. `BodyImage` provides consistent access (reading) to a body and
includes support for memory-mapping a file based body.

## BARC container format

BARC is a minimal container file format for the storage of one or many
HTTP request and response dialogs. A fixed length ASCII-limited record
head specifies lengths of a subsequent series of request and response
header blocks and bodies which are stored as raw (unencoded)
bytes. When not using the internal compression feature, the format is
easily human readable, which makes it ideal as debug output or for
integration tests that need HTTP request/response bodies with headers
as test fixtures or examples.

See the source /sample/*.barc files. (FIXME link)

Other features:

* An additional *meta*-headers block provides more recording details
  and can also be used to store application-specific names/values.

* Concurrent, and sequential or random-access reads by record offset
  (using an external index, such as a database).

* Single-writer sessions are guaranteed safe with N concurrent readers
  (in or out of process).

* Optional per-record gzip or Brotli compression (headers and bodies)

## Dialog

TBD...
