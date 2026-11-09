package oracle

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"sort"
	"sync"
	"time"

	"github.com/cloudwego/kitex/client/callopt"
	"github.com/cloudwego/kitex/client/callopt/streamcall"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
)

func grpcBenchmarkExchange(c service.Client, payload []byte, mode, compression string, phase *grpcPhaseTiming) error {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if phase != nil {
		ctx = context.WithValue(ctx, grpcPhaseKey{}, phase)
	}
	one := int64(1)
	message := &parent.Message{Payload: payload, Sequence: &one}
	var unaryOptions []callopt.Option
	var streamOptions []streamcall.Option
	if compression == "gzip" {
		unaryOptions = append(unaryOptions, callopt.WithGRPCCompressor("gzip"))
		streamOptions = append(streamOptions, streamcall.WithGRPCCompressor("gzip"))
	}
	switch mode {
	case "unary":
		value, err := c.Unary(ctx, message, unaryOptions...)
		if err != nil {
			return err
		}
		phase.receivedMessage()
		if value == nil || !bytes.Equal(value.Payload, payload) || value.GetSequence() != 1 {
			return fmt.Errorf("benchmark unary differs")
		}
	case "client":
		stream, err := c.Client(ctx, streamOptions...)
		if err != nil {
			return err
		}
		for i := 0; i < 3; i++ {
			if err = stream.Send(ctx, message); err != nil {
				return err
			}
		}
		value, err := stream.CloseAndRecv(ctx)
		if err != nil {
			return err
		}
		if value == nil || value.GetSequence() != 3 || len(value.Payload) != len(payload)*3 {
			return fmt.Errorf("benchmark client aggregation differs")
		}
		for i := 0; i < 3; i++ {
			if !bytes.Equal(value.Payload[i*len(payload):(i+1)*len(payload)], payload) {
				return fmt.Errorf("aggregate bytes differ")
			}
		}
	case "server":
		stream, err := c.Server(ctx, message, streamOptions...)
		if err != nil {
			return err
		}
		for i := int64(0); i < 3; i++ {
			value, err := stream.Recv(ctx)
			if err != nil {
				return err
			}
			if value == nil || !bytes.Equal(value.Payload, payload) || value.GetSequence() != i {
				return fmt.Errorf("benchmark server response differs")
			}
		}
		if _, err = stream.Recv(ctx); err != io.EOF {
			return fmt.Errorf("benchmark server terminal: %v", err)
		}
	case "bidi":
		stream, err := c.Bidi(ctx, streamOptions...)
		if err != nil {
			return err
		}
		if err = stream.Send(ctx, message); err != nil {
			return err
		}
		if err = stream.CloseSend(ctx); err != nil {
			return err
		}
		value, err := stream.Recv(ctx)
		if err != nil {
			return err
		}
		if value == nil || !bytes.Equal(value.Payload, payload) || value.GetSequence() != 1 {
			return fmt.Errorf("benchmark bidi differs")
		}
		if _, err = stream.Recv(ctx); err != io.EOF {
			return fmt.Errorf("benchmark bidi terminal: %v", err)
		}
	default:
		return fmt.Errorf("unknown benchmark mode %q", mode)
	}
	return nil
}

func grpcBenchmark(profile Profile, address, mode, compression, report string, seconds, concurrency, payloadBytes int, phaseLatency bool) error {
	if seconds < 1 || seconds > 120 || concurrency < 1 || concurrency > 256 || payloadBytes < 0 || payloadBytes > 1024*1024 ||
		(compression != "identity" && compression != "gzip") {
		return fmt.Errorf("benchmark parameters exceed fixture limits")
	}
	var phases *grpcPhaseRecorder
	if phaseLatency {
		phases = &grpcPhaseRecorder{values: make([]grpcPhaseSample, 0, 8192)}
	}
	client, err := newGRPCClient(profile, address, grpcPhaseOptions(mode, phaseLatency)...)
	if err != nil {
		return err
	}
	payload := bytes.Repeat([]byte{'x'}, payloadBytes)
	for i := 0; i < 32; i++ {
		if err = grpcBenchmarkExchange(client, payload, mode, compression, nil); err != nil {
			return err
		}
	}
	if err = responseEvent(map[string]any{"event": "load-started", "duration_secs": seconds}); err != nil {
		return err
	}
	type result struct {
		calls, errors int
		latencies     []int64
		firstError    string
	}
	results := make(chan result, concurrency)
	begin := time.Now()
	until := begin.Add(time.Duration(seconds) * time.Second)
	var workers sync.WaitGroup
	for worker := 0; worker < concurrency; worker++ {
		workers.Add(1)
		go func() {
			defer workers.Done()
			value := result{latencies: make([]int64, 0, 8192)}
			for time.Now().Before(until) {
				started := time.Now()
				var phase *grpcPhaseTiming
				if phases != nil {
					phase = &grpcPhaseTiming{started: started}
				}
				err := grpcBenchmarkExchange(client, payload, mode, compression, phase)
				if phases != nil {
					phases.finish(phase, err)
				}
				if err != nil {
					value.errors++
					if value.firstError == "" {
						value.firstError = err.Error()
					}
				}
				elapsed := time.Since(started).Microseconds()
				if len(value.latencies) < 8192 {
					value.latencies = append(value.latencies, elapsed)
				} else {
					value.latencies[value.calls%8192] = elapsed
				}
				value.calls++
			}
			results <- value
		}()
	}
	workers.Wait()
	elapsed := time.Since(begin).Seconds()
	close(results)
	var calls, failures int
	var latencies []int64
	var firstError string
	for value := range results {
		calls += value.calls
		failures += value.errors
		latencies = append(latencies, value.latencies...)
		if firstError == "" {
			firstError = value.firstError
		}
	}
	sort.Slice(latencies, func(a, b int) bool { return latencies[a] < latencies[b] })
	status := "PASS"
	if failures != 0 || calls == 0 {
		status = "FAIL"
	}
	percentile := func(percent int) int64 {
		if len(latencies) == 0 {
			return 0
		}
		return latencies[(len(latencies)-1)*percent/100]
	}
	value := map[string]any{"schemaVersion": 1, "event": "result", "status": status, "mode": mode,
		"scope": "real socket capability measurement; no cross-protocol regression claim", "duration_secs": elapsed,
		"concurrency": concurrency, "payload_bytes": payloadBytes, "calls": calls, "errors": failures, "first_error": firstError,
		"qps": float64(calls) / elapsed, "latency_us": map[string]int64{"p50": percentile(50), "p99": percentile(99)},
		"latency_sample_count": len(latencies)}
	if phases != nil {
		value["phase_latencies"] = phases.summary()
	}
	return writeJSON(report, value)
}
