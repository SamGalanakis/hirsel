use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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
            let mut stream = workload::accept_client(&listener).await;
            stream.set_nodelay(true).unwrap();
            let mut buf = Vec::with_capacity(4096);
            for _ in 0..config.requests {
                loop {
                    let mut headers = [httparse::EMPTY_HEADER; 64];
                    let mut req = httparse::Request::new(&mut headers);
                    if let httparse::Status::Complete(end) = req.parse(&buf).unwrap() {
                        assert_eq!(req.method, Some("GET"));
                        assert_eq!(req.path, Some("/"));
                        assert_eq!(req.version, Some(1));
                        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 12\r\n\r\nHello, world").await.unwrap();
                        buf.drain(..end);
                        break;
                    }
                    assert!(buf.len() < 8192, "request head too large");
                    assert!(stream.read_buf(&mut buf).await.unwrap() > 0);
                }
            }
            assert!(buf.is_empty());
            assert_eq!(stream.read_buf(&mut buf).await.unwrap(), 0);
            println!("SERVED {}", config.requests);
        });
}
