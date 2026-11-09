package oracle

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"time"
)

func writeJSON(path string, value any) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	b = append(b, '\n')
	if path != "" {
		if err = os.WriteFile(path, b, 0644); err != nil {
			return err
		}
	}
	_, err = os.Stdout.Write(b)
	return err
}

func Main(profile Profile) {
	role := flag.String("role", "", "golden, codec-verify, payload-matrix, frame-golden, wire, unary-idle, unknown-method, metadata-client, metadata-batch-client, cascade-client, error-client, server, client")
	report := flag.String("report", "", "JSON output path")
	input := flag.String("input", "", "codec fixture or Rust-produced comparison bytes")
	addr := flag.String("addr", "tcp://127.0.0.1:0", "listen or target address")
	mode := flag.String("mode", "bidi", "unary, explicit-unary, client, server, bidi")
	method := flag.String("method", "all", "inherited method or all")
	payload := flag.String("payload", "hello", "request value")
	pool := flag.String("pool", profile.DefaultPool, "short, long, mux")
	api := flag.String("api", "typed", "typed or BinaryThriftGenericV2 binary")
	recvDelay := flag.Duration("recv-delay", 0, "error-client only: intentional application delay before first Recv")
	messages := flag.Int("messages", 3, "number of requests or server responses")
	payloadBytes := flag.Int("payload-bytes", 1024, "budget-client message payload bytes")
	timeout := flag.Duration("timeout", 15*time.Second, "whole call deadline")
	idle := flag.Duration("idle", 5*time.Minute, "unary idle interval before first and reused requests")
	scenario := flag.String("scenario", "echo", "server fixture: echo, metainfo, cascade, errors")
	node := flag.String("node", "C", "multihop fixture node label")
	forward := flag.String("forward", "", "optional multihop downstream address")
	metainfoMode := flag.String("metainfo-mode", "default", "test-only metainfo provider: default, single-hop, legacy")
	serviceName := flag.String("service", "Interop", "fixture service name")
	routingProfile := flag.String("routing-profile", "unique", "routing server profile")
	lifecycleID := flag.String("id", "lifecycle-case", "lifecycle call identity")
	recvTimeoutMS := flag.Int("timeout-ms", 500, "lifecycle per-receive timeout milliseconds")
	flag.Parse()
	var err error
	if *serviceName != "Interop" || *messages < 0 || *recvDelay < 0 || *recvDelay > 3*time.Second || (*api != "typed" && *api != "binary") ||
		(*api == "binary" && *role != "client" && *role != "server") {
		fmt.Fprintln(os.Stderr, "invalid service/count/API combination")
		os.Exit(2)
	}
	switch *role {
	case "inherited-server":
		err = serveInherited(profile, *addr)
	case "inherited-client":
		err = callInherited(profile, *addr, *method, *pool, *report, *messages, *timeout)
	case "golden":
		var result goldenFile
		result, err = makeGolden()
		if err == nil {
			err = writeJSON(*report, result)
		}
	case "codec-verify":
		err = verifyGolden(*input)
		if err == nil {
			err = writeJSON(*report, map[string]any{"schemaVersion": 1, "event": "result", "status": "PASS", "input": *input})
		}
	case "payload-matrix":
		var rows []map[string]any
		rows, err = payloadMatrix()
		if err == nil {
			err = writeJSON(*report, map[string]any{"schemaVersion": 1, "event": "result", "status": "PASS", "cases": rows})
		}
	case "frame-golden":
		var result map[string]any
		result, err = frameGolden()
		if err == nil {
			err = writeJSON(*report, result)
		}
	case "wire":
		err = wire(*addr, *input, *report)
	case "raw-response-server":
		err = rawResponseServer(profile, *addr, *input)
	case "response-client":
		err = responseProbe(profile, *addr, *pool, *report)
	case "unary-idle":
		err = unaryIdle(profile, *addr, *report, *idle, *timeout)
	case "unknown-method":
		err = unknownMethod(profile, *addr, *report)
	case "server":
		err = serve(profile, *addr, serverFixture{API: *api, Scenario: *scenario, Node: *node, Forward: *forward, MetainfoMode: *metainfoMode, RoutingProfile: *routingProfile})
	case "routing-registration":
		err = routingRegistration(profile, *report)
	case "lifecycle-client":
		err = lifecycleCall(profile, *addr, *pool, *scenario, *lifecycleID, *report, time.Duration(*recvTimeoutMS)*time.Millisecond)
	case "shutdown-wire":
		err = shutdownProbe(profile, *addr, *report)
	case "budget-client":
		if *mode != "complete" && *mode != "cancel" {
			err = fmt.Errorf("budget-client mode must be complete or cancel")
		} else {
			err = budgetCall(profile, *addr, *pool, *lifecycleID, *report, *messages, *payloadBytes, *timeout, *mode == "cancel")
		}
	case "metadata-client", "metadata-batch-client":
		err = metadataCall(profile, *addr, *pool, *input, *report, *role == "metadata-batch-client")
	case "cascade-client":
		err = cascadeCall(profile, *addr, *pool, *mode, *payload, *report)
	case "error-client":
		err = errorStatusCall(profile, *addr, *pool, *mode, *payload, *report, *recvDelay)
	case "client":
		if *api == "binary" {
			err = binaryGenericCall(profile, *addr, *mode, *payload, *pool, *report, *messages, *timeout)
		} else {
			err = call(profile, *addr, *mode, *payload, *pool, *report, *messages, *timeout)
		}
	default:
		err = fmt.Errorf("unsupported role %q", *role)
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
