use std::{
    io::{self, Write},
    net::SocketAddr,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    routing::get,
};
use rusqlite::Connection;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(serde::Serialize)]
struct Message {
    message: &'static str,
}

fn open_store(path: &str) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    assert_eq!(mode, "wal");
    db.pragma_update(None, "synchronous", "FULL")?;
    let synchronous: i64 = db.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    assert_eq!(synchronous, 2);
    db.execute_batch(
        "CREATE TABLE counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
         INSERT INTO counter VALUES (1, 0);",
    )?;
    Ok(db)
}

fn increment(db: &mut Connection) -> rusqlite::Result<i64> {
    let read = db.transaction()?;
    let current: i64 =
        read.query_row("SELECT value FROM counter WHERE id = 1", [], |r| r.get(0))?;
    read.commit()?;
    let write = db.transaction()?;
    write.execute("UPDATE counter SET value = ? WHERE id = 1", [current + 1])?;
    write.commit()?;
    Ok(current + 1)
}

async fn serve_store(listener: TcpListener, mut db: Connection) -> io::Result<()> {
    loop {
        let (mut stream, address) = listener.accept().await?;
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), stream.read_u8()).await,
            Ok(Ok(0xa3))
        ) {
            eprintln!("skipped unexpected loopback connection from {address}");
            continue;
        }
        match increment(&mut db) {
            Ok(value) => {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    stream.write_all(&value.to_le_bytes()),
                )
                .await;
            }
            Err(error) => eprintln!("{error}"),
        }
    }
}

async fn store(
    State(address): State<SocketAddr>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let value = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await?;
        stream.write_u8(0xa3).await?;
        stream.read_u64_le().await
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(([(header::CONTENT_TYPE, "text/plain")], value.to_string()))
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("expected a fresh SQLite path");
    let db = open_store(&path).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let place = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let place_address = place.local_addr().unwrap();
            tokio::spawn(async move {
                if let Err(error) = serve_store(place, db).await {
                    eprintln!("{error}");
                }
            });
            let app = Router::new()
                .route(
                    "/plaintext",
                    get(|| async { ([(header::CONTENT_TYPE, "text/plain")], "Hello, world") }),
                )
                .route(
                    "/json",
                    get(|| async {
                        Json(Message {
                            message: "Hello, world",
                        })
                    }),
                )
                .route("/store", get(store))
                .with_state(place_address);
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            println!("LISTEN {}", listener.local_addr().unwrap());
            io::stdout().flush().unwrap();
            axum::serve(listener, app).await.unwrap();
        });
}
