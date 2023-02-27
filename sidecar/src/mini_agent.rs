use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use ddtelemetry::data::Payload;
use hyper::body::HttpBody;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use hyperlocal::{SocketIncoming, UnixServerExt};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;

use crate::connections::UnixListenerTracked;

#[derive(Debug, Deserialize, Serialize)]
pub struct Span<'a> {
    #[serde(borrow)]
    service: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Cow<'a, str>,
    resource: Cow<'a, str>,
    trace_id: u64,
    span_id: u64,
    parent_id: Option<u64>,
    start: i64,
    duration: i64,
    error: i32,
    #[serde(borrow)]
    meta: HashMap<&'a str, &'a str>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct V04Trace<'a> {
    #[serde(borrow)]
    spans: Vec<Span<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct V04Payload<'a> {
    #[serde(borrow)]
    traces: Vec<V04Trace<'a>>,
}

// Example traced app: go install github.com/DataDog/trace-examples/go/heartbeat@latest

async fn echo(mut req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    match (req.method(), req.uri().path()) {
        // exit, shutting down the subprocess process.
        (&Method::GET, "/exit") => {
            std::process::exit(0);
        }
        (&Method::POST, "/v0.4/traces") => {
            let body = hyper::body::to_bytes(req.body_mut()).await.unwrap();
            let body: V04Payload = rmp_serde::from_slice(&body).unwrap();

            eprintln!("Traces received: {:?}", body);

            Ok(Response::default())
        }

        // Return the 404 Not Found for other routes.
        _ => {
            let body = hyper::body::to_bytes(req.body_mut()).await.unwrap();
            eprintln!("{} called {:?}, body: {:?}", req.uri(), req.headers(), body);

            let mut not_found = Response::default();
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

pub(crate) async fn main(
    listener: UnixListener,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = make_service_fn(|_| async { Ok::<_, hyper::Error>(service_fn(echo)) });
    let listener = UnixListenerTracked::from(listener);
    let watcher = listener.watch();

    let server = Server::builder(listener).serve(service);
    tokio::select! {
        // if there are no connections for 1 second, exit the main loop
        _ = watcher.wait_for_no_instances(Duration::from_secs(1)) => {
            Ok(())
        }
        res = server => {
            res?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, collections::HashMap};

    use crate::mini_agent::{Span, V04Payload, V04Trace};

    #[test]
    fn test_borrow_when_deserializing() {
        let data_orig = V04Payload {
            traces: vec![V04Trace {
                spans: vec![Span {
                    service: Some("service".into()),
                    name: "name".into(),
                    resource: "resource".into(),
                    trace_id: 1,
                    span_id: 2,
                    parent_id: None,
                    start: 4,
                    duration: 5,
                    error: 1,
                    meta: HashMap::from([("key", "value")]),
                }],
            }],
        };

        let buf = rmp_serde::to_vec(&data_orig).expect("serialize");
        let data_new: V04Payload = rmp_serde::from_slice(&buf).expect("deserialize");

        // Validate data in deserialized payload, is borrowed from buffer
        // where possible to avoid unnecessary allocations when processing incoming data
        //
        // note, serde borrow deserialization has some edgecases with nested types
        // best to check if things are actually borrowed here. 
        let span = &data_new.traces[0].spans[0];

        // Option<Cow> is not borrowed by default
        // TODO: use https://docs.rs/serde_with/latest/serde_with/struct.BorrowCow.html or remove Cow
        assert!(matches!(span.service, Some(Cow::Owned(_))));

        assert!(buf.as_ptr_range().contains(&span.name.as_ptr()));
        assert!(matches!(span.name, Cow::Borrowed(_)));
        assert!(matches!(span.resource, Cow::Borrowed(_)));

        for (k, v) in &span.meta {
            assert!(buf.as_ptr_range().contains(&k.as_ptr()));
            assert!(buf.as_ptr_range().contains(&v.as_ptr()));
        }
    }
}
