#[macro_use] extern crate failure;
extern crate futures;
extern crate hyper;
extern crate tempfile;
extern crate tokio_core;

// FIXME: Use atleast while prototyping. Might eventually switch to an
// error enum to get clear seperation between hyper::Error and
// application errors.
use failure::Error as FlError;

use std::io::Write;
use futures::{Future, Stream};
use futures::future::err as futerr;
use hyper::Client;
use hyper::client::{FutureResponse, Response};
use tokio_core::reactor::Core;
use tempfile::tempfile;

// FIXME: Alt. names: Hyperbowl or hyperbole
pub struct BarcWriter {}

impl BarcWriter {
    fn resp_future(&mut self, res: Response)
        -> Box<Future<Item=usize, Error=FlError> + Send>
    {
        println!("Response: {}", res.status());
        println!("Headers:\n{}", res.headers());

        match tempfile() {
            Ok(mut tfile) => {
                let s = res.body().map_err(FlError::from).
                    fold(0, move |len_read, chunk| {
                        let new_len = len_read + chunk.len();
                        if new_len > 50_000 {
                            bail!("Response stream too long: {}+", new_len);
                        } else {
                            println!("to read chunk ({})", chunk.len());
                            tfile.write_all(&chunk).
                                map_err(FlError::from).
                                and(Ok(new_len))
                        }
                    });
                Box::new(s)
            }
            Err(e) => Box::new(futerr(e.into()))
        }
    }

    pub fn example(&mut self) -> Result<usize, FlError> {
        let mut core = Core::new()?;
        let client = Client::new(&core.handle());

        // hyper::uri::Uri, via std String parse and FromStr
        let uri = "http://gravitext.com".parse()?;

        let fr: FutureResponse = client.get(uri);

        let work = fr.
            map_err(FlError::from).
            // -----(FnOnce(Response) -> IntoFuture<Error=FlError>)
            and_then(|res| self.resp_future(res));

        let len = core.run(work)?;
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_example() {
        let mut bw = BarcWriter {};

        match bw.example() {
            Ok(len) => println!("Read: {} byte body", len),
            Err(e) => panic!("Error from work: {:?}", e)
        }
    }
}
