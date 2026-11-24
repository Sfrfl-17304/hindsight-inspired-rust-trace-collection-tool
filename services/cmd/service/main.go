// A small configurable microservice. The same binary plays frontend, orders
// and payments; the call graph comes from DOWNSTREAM.
//
// For every request it records one span and hands it to its own Rust agent
// on localhost (same network namespace = same node). It also leaves
// breadcrumbs: the agent addresses of its caller and of every callee, which
// the agents later follow when a trace is triggered.
package main

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	mrand "math/rand/v2"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync/atomic"
	"time"
)

const (
	hTrace  = "X-Rewind-Trace"  // trace id
	hParent = "X-Rewind-Parent" // caller's span id
	hAgent  = "X-Rewind-Agent"  // agent address of the sender (request) or responder (response)
	// Number of services that handled the request, this one included. Only
	// used by the load generator to know how many spans a trace should have.
	hHops = "X-Rewind-Hops"
)

type Span struct {
	TraceID     string   `json:"trace_id"`
	SpanID      string   `json:"span_id"`
	ParentID    string   `json:"parent_id,omitempty"`
	Node        string   `json:"node"`
	Name        string   `json:"name"`
	StartUnixUs int64    `json:"start_unix_us"`
	DurationUs  int64    `json:"duration_us"`
	Error       bool     `json:"error"`
	Peers       []string `json:"peers,omitempty"`
}

type server struct {
	node       string
	agentAddr  string // how other agents reach this node's agent, e.g. "orders:9000"
	downstream []string
	failRate   float64
	work       time.Duration
	client     *http.Client
	exp        *exporter
}

func main() {
	s := &server{
		node:       mustEnv("NODE_NAME"),
		agentAddr:  mustEnv("AGENT_ADDR"),
		downstream: splitList(os.Getenv("DOWNSTREAM")),
		failRate:   envFloat("FAIL_RATE", 0),
		work:       time.Duration(envFloat("WORK_MS", 5) * float64(time.Millisecond)),
		client:     &http.Client{Timeout: 2 * time.Second},
		exp:        newExporter(envOr("AGENT_URL", "http://127.0.0.1:9000")),
	}
	go s.exp.run()

	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) { fmt.Fprintln(w, "ok") })
	mux.HandleFunc("/", s.handle)

	listen := envOr("LISTEN", ":8080")
	log.Printf("%s listening on %s, downstream=%v, fail_rate=%.4f", s.node, listen, s.downstream, s.failRate)
	log.Fatal(http.ListenAndServe(listen, mux))
}

func (s *server) handle(w http.ResponseWriter, r *http.Request) {
	start := time.Now()
	traceID := r.Header.Get(hTrace)
	if traceID == "" {
		traceID = newID(16)
	}
	sp := Span{
		TraceID:  traceID,
		SpanID:   newID(8),
		ParentID: r.Header.Get(hParent),
		Node:     s.node,
		Name:     r.Method + " " + r.URL.Path,
	}
	if caller := r.Header.Get(hAgent); caller != "" {
		sp.Peers = append(sp.Peers, caller)
	}
	w.Header().Set(hAgent, s.agentAddr)
	w.Header().Set(hTrace, traceID)

	time.Sleep(s.work) // simulated work

	failed := false
	hops := 1
	for _, d := range s.downstream {
		peer, n, err := s.call(d, traceID, sp.SpanID)
		hops += n
		if peer != "" {
			sp.Peers = append(sp.Peers, peer)
		}
		if err != nil {
			failed = true
			break
		}
	}
	if !failed && s.failRate > 0 && mrand.Float64() < s.failRate {
		failed = true
	}

	sp.Error = failed
	sp.StartUnixUs = start.UnixMicro()
	sp.DurationUs = time.Since(start).Microseconds()
	s.exp.add(sp)

	w.Header().Set(hHops, strconv.Itoa(hops))
	if failed {
		http.Error(w, s.node+" failed", http.StatusInternalServerError)
		return
	}
	fmt.Fprintf(w, "ok from %s\n", s.node)
}

// call forwards the request downstream. It returns the callee's agent address
// and the number of services that handled the request downstream (0 if the
// callee could not be reached or did not answer in time).
func (s *server) call(url, traceID, parentSpan string) (string, int, error) {
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return "", 0, err
	}
	req.Header.Set(hTrace, traceID)
	req.Header.Set(hParent, parentSpan)
	req.Header.Set(hAgent, s.agentAddr)
	resp, err := s.client.Do(req)
	if err != nil {
		return "", 0, err
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	peer := resp.Header.Get(hAgent)
	hops, _ := strconv.Atoi(resp.Header.Get(hHops))
	if resp.StatusCode >= 500 {
		return peer, hops, fmt.Errorf("%s returned %d", url, resp.StatusCode)
	}
	return peer, hops, nil
}

// exporter batches spans and posts them to the local agent. It never blocks
// a request: if the queue is full the span is dropped and counted.
type exporter struct {
	url     string
	ch      chan Span
	client  *http.Client
	dropped atomic.Int64
}

func newExporter(agentURL string) *exporter {
	return &exporter{
		url:    strings.TrimRight(agentURL, "/") + "/v1/spans",
		ch:     make(chan Span, 10_000),
		client: &http.Client{Timeout: time.Second},
	}
}

func (e *exporter) add(s Span) {
	select {
	case e.ch <- s:
	default:
		e.dropped.Add(1)
	}
}

func (e *exporter) run() {
	tick := time.NewTicker(50 * time.Millisecond)
	batch := make([]Span, 0, 256)
	flush := func() {
		if n := e.dropped.Swap(0); n > 0 {
			log.Printf("exporter queue full, dropped %d spans", n)
		}
		if len(batch) == 0 {
			return
		}
		body, _ := json.Marshal(batch)
		resp, err := e.client.Post(e.url, "application/json", bytes.NewReader(body))
		if err != nil {
			log.Printf("export to agent failed: %v", err)
		} else {
			resp.Body.Close()
		}
		batch = batch[:0]
	}
	for {
		select {
		case s := <-e.ch:
			batch = append(batch, s)
			if len(batch) >= 256 {
				flush()
			}
		case <-tick.C:
			flush()
		}
	}
}

func newID(nBytes int) string {
	b := make([]byte, nBytes)
	rand.Read(b)
	return hex.EncodeToString(b)
}

func mustEnv(k string) string {
	v := os.Getenv(k)
	if v == "" {
		log.Fatalf("missing env var %s", k)
	}
	return v
}

func envOr(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func envFloat(k string, def float64) float64 {
	v, err := strconv.ParseFloat(os.Getenv(k), 64)
	if err != nil {
		return def
	}
	return v
}

func splitList(s string) []string {
	var out []string
	for _, p := range strings.Split(s, ",") {
		if p = strings.TrimSpace(p); p != "" {
			out = append(out, p)
		}
	}
	return out
}
