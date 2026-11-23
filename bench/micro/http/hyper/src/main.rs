use std::{cell::Cell, convert::Infallible, rc::Rc};

use http_body_util::Full;
use hyper::{Request, Response, body::Bytes, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

#[path = "../../workload.rs"]
mod workload;

fn main() {
    let config = workload::Config::read();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap()
        .block_on(async {
            let listener = TcpListener::bind(("127.0.0.1", config.port)).await.unwrap();
            workload::announce(listener.local_addr().unwrap());
            let stream = workload::accept_client(&listener).await;
            stream.set_nodelay(true).unwrap();
            let served = Rc::new(Cell::new(0));
            let count = served.clone();
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                assert_eq!(request.method(), "GET");
                assert_eq!(request.uri(), "/");
                assert_eq!(request.version(), hyper::Version::HTTP_11);
                count.set(count.get() + 1);
                async {
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("content-type", "text/plain")
                            .header("content-length", "12")
                            .body(Full::new(Bytes::from_static(b"Hello, world")))
                            .unwrap(),
                    )
                }
            });
            http1::Builder::new()
                .auto_date_header(true)
                .title_case_headers(true)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
            assert_eq!(served.get(), config.requests);
            println!("SERVED {}", served.get());
        });
}
