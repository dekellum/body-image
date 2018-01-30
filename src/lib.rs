#![allow(dead_code)]

extern crate failure; // #[macro_use]
extern crate futures;
extern crate hyper;
extern crate tempfile;
extern crate tokio_core;

use failure::Error;

use std::io::{Error as IoError, ErrorKind, Write};
use futures::{Future, Stream};
use futures::future::err as futerr;
use hyper::Client;
use hyper::client::{FutureResponse, Response};
use tokio_core::reactor::Core;
use tempfile::tempfile;

struct BarcWriter {}

impl BarcWriter {
    fn resp_future(&mut self, res: Response)
        -> Box<Future<Item=(), Error=hyper::Error> + Send>
    {
        println!("Response: {}", res.status());
        println!("Headers:\n{}", res.headers());

        match tempfile() {
            Ok(mut tfile) => {
                let mut length_read: usize = 0;
                Box::new(res.body().for_each(move |chunk| {
                    length_read += chunk.len();
                    if length_read > 5_000 {
                        Err(IoError::new(
                            ErrorKind::Other,
                            format!("too long: {}+", length_read)
                        ).into())
                    } else {
                        println!("chunk ({})", length_read);
                        tfile.write_all(&chunk).map_err(From::from)
                    }
                }))
            }
            Err(e) => Box::new(futerr::<(), _>(e.into()))
        }
    }

    fn example(&mut self) -> Result<(), Error> {
        let mut core = Core::new()?;
        let client = Client::new(&core.handle());

        // hyper::uri::Uri, via std String parse and FromStr
        let uri = "http://gravitext.com".parse()?;

        let fr: FutureResponse = client.get(uri); // FutureResponse

        // FnOnce(Response) -> IntoFuture<Error=hyper::Error>
        let work = fr.and_then(|res| self.resp_future(res));

        core.run(work)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_example() {
        let mut bw = BarcWriter {};

        match bw.example() {
            Ok(_) => println!("ok"),
            Err(e) => panic!("Error from work: {:?}", e)
        }
    }
}
