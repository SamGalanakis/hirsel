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
