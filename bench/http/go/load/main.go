package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"runtime"
	"strconv"
	"time"

	hdr "github.com/HdrHistogram/hdrhistogram-go"
)

const requestTimeout = 5 * time.Second

type target struct {
	address string
	path    string
	wire    string
}

type sample struct {
	latency        time.Duration
	sentLate       time.Duration
	completed      bool
	beforeStop     bool
	requestFailure string
}

func (t target) verify(response *http.Response) error {
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK || response.Close ||
		response.ContentLength < 1 || response.ContentLength > 128 {
		return fmt.Errorf("unexpected response status or framing: %s", response.Status)
	}
	var body [128]byte
	if _, err := io.ReadFull(response.Body, body[:response.ContentLength]); err != nil {
		return err
	}
	content := string(body[:response.ContentLength])
	kind := response.Header.Get("Content-Type")
	switch t.path {
	case "/plaintext":
		if kind == "text/plain" && content == "Hello, world" {
			return nil
		}
	case "/json":
		if kind == "application/json" && content == `{"message":"Hello, world"}` {
			return nil
		}
	case "/store":
		if value, err := strconv.ParseUint(content, 10, 64); err == nil && value > 0 && kind == "text/plain" {
			return nil
		}
	}
	return fmt.Errorf("unexpected %s response: %q %q", t.path, kind, content)
}

func (t target) exchange(conn net.Conn, reader *bufio.Reader, at, stop time.Time) (sample, error) {
	deadline := at.Add(requestTimeout)
	if err := conn.SetDeadline(deadline); err != nil {
		return sample{}, err
	}
	late := time.Since(at)
	if _, err := io.WriteString(conn, t.wire); err != nil {
		return sample{}, err
	}
	response, err := http.ReadResponse(reader, nil)
	if err != nil {
		return sample{}, err
	}
	if err := t.verify(response); err != nil {
		return sample{}, err
	}
	now := time.Now()
	return sample{latency: now.Sub(at), sentLate: late, completed: true, beforeStop: now.Before(stop)}, nil
}

func worker(t target, initial net.Conn, jobs <-chan time.Time, out chan<- sample, stop time.Time) {
	conn := initial
	reader := bufio.NewReader(conn)
	defer func() {
		if conn != nil {
			conn.Close()
		}
	}()
	for scheduled := range jobs {
		left := scheduled.Add(requestTimeout).Sub(time.Now())
		if left <= 0 {
			out <- sample{requestFailure: "arrival expired in client queue"}
			continue
		}
		if conn == nil {
			var err error
			conn, err = net.DialTimeout("tcp4", t.address, left)
			if err != nil {
				out <- sample{requestFailure: err.Error()}
				continue
			}
			reader.Reset(conn)
		}
		observation, err := t.exchange(conn, reader, scheduled, stop)
		if err != nil {
			conn.Close()
			conn = nil
			observation.requestFailure = err.Error()
		}
		out <- observation
	}
	out <- sample{requestFailure: ""}
}

type result struct {
	Offered      int
	Completed    int
	OnTime       int
	Dropped      int
	Failed       int
	FirstFailure string
	P50NS        int64
	P99NS        int64
	P999NS       int64
	MaxNS        int64
	SendP99NS    int64
}

func collect(samples <-chan sample, workers int, done chan<- result) {
	histogram := hdr.New(1, int64(requestTimeout), 3)
	sendDelay := hdr.New(1, int64(requestTimeout), 3)
	var r result
	for workers > 0 {
		s := <-samples
		switch {
		case s.completed:
			r.Completed++
			if s.beforeStop {
				r.OnTime++
			}
			r.MaxNS = max(r.MaxNS, int64(s.latency))
			if histogram.RecordValue(int64(s.latency)) != nil ||
				sendDelay.RecordValue(max(1, int64(s.sentLate))) != nil {
				r.Failed++
				r.FirstFailure = "sample exceeded request timeout"
			}
		case s.requestFailure == "":
			workers--
		default:
			r.Failed++
			if r.FirstFailure == "" {
				r.FirstFailure = s.requestFailure
			}
		}
	}
	r.P50NS = histogram.ValueAtQuantile(50)
	r.P99NS = histogram.ValueAtQuantile(99)
	r.P999NS = histogram.ValueAtQuantile(99.9)
	r.SendP99NS = sendDelay.ValueAtQuantile(99)
	done <- r
}

func run(t target, rate, seconds, workers int) (result, error) {
	connections := make([]net.Conn, 0, workers)
	for range workers {
		conn, err := net.DialTimeout("tcp4", t.address, requestTimeout)
		if err != nil {
			for _, previous := range connections {
				previous.Close()
			}
			return result{}, err
		}
		connections = append(connections, conn)
	}
	jobs := make(chan time.Time, 4096)
	samples := make(chan sample, workers)
	done := make(chan result, 1)
	start := time.Now().Add(250 * time.Millisecond)
	stop := start.Add(time.Duration(seconds) * time.Second)
	go collect(samples, workers, done)
	for _, conn := range connections {
		go worker(t, conn, jobs, samples, stop)
	}
	offered := rate * seconds
	dropped := 0
	for index := 0; index < offered; index++ {
		scheduled := start.Add(time.Duration(int64(index) * int64(time.Second) / int64(rate)))
		if delay := time.Until(scheduled); delay > 0 {
			time.Sleep(delay)
		}
		select {
		case jobs <- scheduled:
		default:
			dropped++
		}
	}
	close(jobs)
	r := <-done
	r.Offered, r.Dropped = offered, dropped
	if r.Completed == 0 {
		return r, errors.New("no complete responses")
	}
	return r, nil
}

func main() {
	runtime.GOMAXPROCS(1)
	address := flag.String("address", "", "loopback listener address")
	route := flag.String("route", "", "plaintext, json or store")
	rate := flag.Int("rate", 0, "fixed arrivals per second")
	seconds := flag.Int("seconds", 0, "duration of the arrival schedule")
	workers := flag.Int("connections", 64, "number of persistent TCP connections")
	flag.Parse()
	if *address == "" || *rate < 1 || *rate > 1_000_000 || *seconds < 1 ||
		*seconds > 7200 || *workers < 1 || *workers > 256 {
		log.Fatal("invalid load configuration")
	}
	switch *route {
	case "plaintext", "json", "store":
	default:
		log.Fatal("unknown route")
	}
	path := "/" + *route
	r, err := run(target{*address, path,
		"GET " + path + " HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n"},
		*rate, *seconds, *workers)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
	}
	if err := json.NewEncoder(os.Stdout).Encode(r); err != nil {
		log.Fatal(err)
	}
}
