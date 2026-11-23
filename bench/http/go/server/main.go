package main

import (
	"database/sql"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"runtime"
	"time"

	_ "github.com/mattn/go-sqlite3"
)

type message struct {
	Message string `json:"message"`
}

func increment(db *sql.DB) (int64, error) {
	read, err := db.Begin()
	if err != nil {
		return 0, err
	}
	defer read.Rollback()
	var value int64
	if err := read.QueryRow("SELECT value FROM counter WHERE id = 1").Scan(&value); err != nil {
		return 0, err
	}
	if err := read.Commit(); err != nil {
		return 0, err
	}
	write, err := db.Begin()
	if err != nil {
		return 0, err
	}
	defer write.Rollback()
	if _, err := write.Exec("UPDATE counter SET value = ? WHERE id = 1", value+1); err != nil {
		return 0, err
	}
	return value + 1, write.Commit()
}

func serveStore(listener net.Listener, db *sql.DB) {
	for {
		client, err := listener.Accept()
		if err != nil {
			log.Fatal(err)
		}
		var hello [1]byte
		client.SetReadDeadline(time.Now().Add(5 * time.Second))
		if _, err := io.ReadFull(client, hello[:]); err != nil || hello[0] != 0xa3 {
			log.Printf("skipped unexpected loopback connection from %s", client.RemoteAddr())
			client.Close()
			continue
		}
		value, err := increment(db)
		if err != nil {
			log.Println(err)
		} else {
			var reply [8]byte
			binary.LittleEndian.PutUint64(reply[:], uint64(value))
			client.SetWriteDeadline(time.Now().Add(5 * time.Second))
			client.Write(reply[:])
		}
		client.Close()
	}
}

func storeValue(address string) (uint64, error) {
	deadline := time.Now().Add(5 * time.Second)
	conn, err := net.DialTimeout("tcp4", address, time.Until(deadline))
	if err != nil {
		return 0, err
	}
	defer conn.Close()
	if err := conn.SetWriteDeadline(deadline); err != nil {
		return 0, err
	}
	if _, err := conn.Write([]byte{0xa3}); err != nil {
		return 0, err
	}
	if err := conn.SetReadDeadline(deadline); err != nil {
		return 0, err
	}
	var reply [8]byte
	_, err = io.ReadFull(conn, reply[:])
	return binary.LittleEndian.Uint64(reply[:]), err
}

func openStore(path string) *sql.DB {
	db, err := sql.Open("sqlite3", path+"?_journal_mode=WAL&_synchronous=FULL")
	if err != nil {
		log.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	var mode string
	var synchronous int
	if err := db.QueryRow("PRAGMA journal_mode").Scan(&mode); err != nil || mode != "wal" {
		log.Fatalf("SQLite journal_mode = %q, %v", mode, err)
	}
	if err := db.QueryRow("PRAGMA synchronous").Scan(&synchronous); err != nil || synchronous != 2 {
		log.Fatalf("SQLite synchronous = %d, %v", synchronous, err)
	}
	_, err = db.Exec(`CREATE TABLE counter (id INTEGER PRIMARY KEY, value INTEGER NOT NULL);
INSERT INTO counter VALUES (1, 0)`)
	if err != nil {
		log.Fatal(err)
	}
	return db
}

func observeAllocations(file *os.File) {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for range ticker.C {
		var stats runtime.MemStats
		runtime.ReadMemStats(&stats)
		var buf [20]byte
		copy(buf[:], fmt.Sprintf("%020d", stats.Mallocs))
		if _, err := file.WriteAt(buf[:], 0); err != nil {
			log.Fatal(err)
		}
	}
}

func main() {
	runtime.GOMAXPROCS(1)
	if len(os.Args) != 2 {
		log.Fatal("expected a fresh SQLite path")
	}
	db := openStore(os.Args[1])
	defer db.Close()
	place, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		log.Fatal(err)
	}
	defer place.Close()
	go serveStore(place, db)
	placeAddress := place.Addr().String()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /plaintext", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		io.WriteString(w, "Hello, world")
	})
	mux.HandleFunc("GET /json", func(w http.ResponseWriter, _ *http.Request) {
		body, err := json.Marshal(message{Message: "Hello, world"})
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Write(body)
	})
	mux.HandleFunc("GET /store", func(w http.ResponseWriter, _ *http.Request) {
		value, err := storeValue(placeAddress)
		if err != nil {
			log.Println(err)
			http.Error(w, "store operation failed", http.StatusServiceUnavailable)
			return
		}
		w.Header().Set("Content-Type", "text/plain")
		fmt.Fprint(w, value)
	})
	if path := os.Getenv("HTTP_BENCH_GO_ALLOC"); path != "" {
		file, err := os.Create(path)
		if err != nil {
			log.Fatal(err)
		}
		defer file.Close()
		go observeAllocations(file)
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		log.Fatal(err)
	}
	fmt.Println("LISTEN", listener.Addr())
	server := http.Server{Handler: mux, ReadHeaderTimeout: 10 * time.Second}
	log.Fatal(server.Serve(listener))
}
