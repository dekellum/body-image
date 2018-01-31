#![allow(dead_code)]

#[macro_use] extern crate failure;
extern crate futures;
extern crate hyper;
extern crate tempfile;
extern crate tokio_core;

use failure::Error as FlError;

use std::io::Write;
use futures::{Future, Stream};
use futures::future::err as futerr;
use hyper::Client;
use hyper::client::{FutureResponse, Response};
use tokio_core::reactor::Core;
use tempfile::tempfile;

struct BarcWriter {}

impl BarcWriter {
    fn resp_future(&mut self, res: Response)
        -> Box<Future<Item=(), Error=FlError> + Send>
    {
        println!("Response: {}", res.status());
        println!("Headers:\n{}", res.headers());

        match tempfile() {
            Ok(mut tfile) => {
                let mut length_read: usize = 0;
                let s = res.body().map_err(FlError::from).
                    for_each(move |chunk| {
                        length_read += chunk.len();
                        if length_read > 50_000 {
                            bail!("Response stream too long: {}+", length_read);
                        } else {
                            println!("chunk ({})", length_read);
                            tfile.write_all(&chunk).map_err(FlError::from)
                        }
                    });
                Box::new(s)
            }
            Err(e) => Box::new(futerr::<(), _>(e.into()))
        }
    }

    fn example(&mut self) -> Result<(), FlError> {
        let mut core = Core::new()?;
        let client = Client::new(&core.handle());

        // hyper::uri::Uri, via std String parse and FromStr
        let uri = "http://gravitext.com".parse()?;

        let fr: FutureResponse = client.get(uri); // FutureResponse

        // FnOnce(Response) -> IntoFuture<Error=hyper::Error>
        let work = fr.map_err(FlError::from).and_then(|res| self.resp_future(res));

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
