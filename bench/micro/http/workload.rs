use std::io::Write;

pub struct Config {
    pub requests: usize,
    pub port: u16,
}

impl Config {
    pub fn read() -> Self {
        let mut config = Self {
            requests: 1024,
            port: 0,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = args.next().expect("expected a value after the flag");
            match flag.as_str() {
                "--requests" => config.requests = value.parse().expect("invalid request count"),
                "--port" => config.port = value.parse().expect("invalid port"),
                _ => panic!("unknown flag: {flag}"),
            }
        }
        assert!(config.requests > 0);
        config
    }
}

pub fn announce(address: std::net::SocketAddr) {
    println!("LISTEN {address}");
    std::io::stdout().flush().unwrap();
}

pub async fn accept_client(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    loop {
        let (socket, address) = listener.accept().await.unwrap();
        if address.ip() == std::net::Ipv4Addr::new(127, 0, 0, 2) {
            return socket;
        }
        eprintln!("skipped unexpected loopback connection from {address}");
    }
}
