// Load generator. It picks the trace id of every request itself, so the
// output CSV is the ground truth: which traces failed or were slow. The
// experiments compare this file with what the collector actually received.
package main

import (
	"crypto/rand"
	"encoding/csv"
	"encoding/hex"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"
)

type result struct {
	traceID   string
	start     time.Time
	status    int
	latencyMs float64
}

func main() {
	url := flag.String("url", "http://frontend:8080/checkout", "target URL")
	rps := flag.Float64("rps", 50, "requests per second")
	dur := flag.Duration("duration", 30*time.Second, "how long to run")
	out := flag.String("out", "ground_truth.csv", "CSV output path")
	flag.Parse()

	client := &http.Client{Timeout: 5 * time.Second}
	var (
		mu      sync.Mutex
		results []result
		wg      sync.WaitGroup
	)

	tick := time.NewTicker(time.Duration(float64(time.Second) / *rps))
	defer tick.Stop()
	deadline := time.After(*dur)

loop:
	for {
		select {
		case <-deadline:
			break loop
		case <-tick.C:
			wg.Add(1)
			go func() {
				defer wg.Done()
				r := result{traceID: newID(), start: time.Now()}
				req, _ := http.NewRequest(http.MethodGet, *url, nil)
				req.Header.Set("X-Rewind-Trace", r.traceID)
				resp, err := client.Do(req)
				if err != nil {
					r.status = 0 // transport error
				} else {
					io.Copy(io.Discard, resp.Body)
					resp.Body.Close()
					r.status = resp.StatusCode
				}
				r.latencyMs = float64(time.Since(r.start).Microseconds()) / 1000
				mu.Lock()
				results = append(results, r)
				mu.Unlock()
			}()
		}
	}
	wg.Wait()

	f, err := os.Create(*out)
	if err != nil {
		log.Fatal(err)
	}
	defer f.Close()
	w := csv.NewWriter(f)
	w.Write([]string{"trace_id", "start_unix_us", "status", "latency_ms"})
	failed := 0
	for _, r := range results {
		if r.status != 200 {
			failed++
		}
		w.Write([]string{r.traceID, strconv.FormatInt(r.start.UnixMicro(), 10),
			strconv.Itoa(r.status), strconv.FormatFloat(r.latencyMs, 'f', 2, 64)})
	}
	w.Flush()
	fmt.Printf("sent %d requests, %d failed, ground truth written to %s\n", len(results), failed, *out)
}

func newID() string {
	b := make([]byte, 16)
	rand.Read(b)
	return hex.EncodeToString(b)
}
