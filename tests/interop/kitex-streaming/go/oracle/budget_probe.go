package oracle

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/kerrors"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	"github.com/cloudwego/kitex/server"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

// The application gate intentionally does not call Recv after its acknowledgment.
// It controls consumption only; Kitex still owns frame decoding and its unbounded
// receive queue. Release is a separate stdin control, never a sleep or payload
// that would require the blocked application to consume the stream first.
type budgetGate struct {
	release chan struct{}
	once    sync.Once
}

type budgetHandler struct {
	handler
	mu     sync.Mutex
	gates  map[string]*budgetGate
	active atomic.Int64
}

func (h *budgetHandler) controls(reader io.Reader) {
	scanner := bufio.NewScanner(reader)
	scanner.Buffer(make([]byte, 4096), 64*1024)
	for scanner.Scan() {
		var request lifecycleRequest
		if err := json.Unmarshal(scanner.Bytes(), &request); err != nil || request.Action != "release" || request.ID == "" {
			lifecycleEvent(map[string]any{"event": "budget_control_error", "error": "expected {action:release,id:nonempty}"})
			continue
		}
		h.mu.Lock()
		gate := h.gates[request.ID]
		h.mu.Unlock()
		if gate != nil {
			gate.once.Do(func() { close(gate.release) })
		}
		// A peer RST may have already removed the gate. Report this observation
		// rather than constructing a new gate or pretending release succeeded.
		lifecycleEvent(map[string]any{"event": "budget_release", "id": request.ID, "found": gate != nil})
	}
	if err := scanner.Err(); err != nil {
		lifecycleEvent(map[string]any{"event": "budget_control_error", "error": err.Error()})
	}
}

func (h *budgetHandler) Bidi(ctx context.Context, stream message.Interop_BidiServer) (err error) {
	first, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	var request lifecycleRequest
	if err = json.Unmarshal([]byte(first.Value), &request); err != nil {
		return err
	}
	if first.Seq != 1 || request.SchemaVersion != 1 || request.ID == "" || (request.Action != "hold" && request.Action != "probe") {
		return fmt.Errorf("invalid budget request")
	}
	if request.Action == "probe" {
		return stream.Send(ctx, first)
	}
	gate := &budgetGate{release: make(chan struct{})}
	h.mu.Lock()
	if h.gates == nil {
		h.gates = make(map[string]*budgetGate)
	}
	if _, exists := h.gates[request.ID]; exists {
		h.mu.Unlock()
		return fmt.Errorf("duplicate active budget id %q", request.ID)
	}
	h.gates[request.ID] = gate
	h.mu.Unlock()
	h.active.Add(1)
	var received int32
	var receivedBytes int64
	defer func() {
		h.mu.Lock()
		if h.gates[request.ID] == gate {
			delete(h.gates, request.ID)
		}
		gates := len(h.gates)
		h.mu.Unlock()
		lifecycleEvent(map[string]any{"event": "observed", "id": request.ID, "terminal": budgetTerminal(err),
			"active_handlers": h.active.Add(-1), "gates": gates, "received": received, "received_bytes": receivedBytes})
	}()
	var connection any
	if info := rpcinfo.GetRPCInfo(ctx); info != nil && info.From().Address() != nil {
		connection = info.From().Address().String()
	}
	lifecycleEvent(map[string]any{"event": "started", "id": request.ID, "connection": connection, "held": true})
	ack, err := lifecycleMessage("ack", request.ID)
	if err != nil {
		return err
	}
	if err = stream.Send(ctx, ack); err != nil {
		return err
	}
	select {
	case <-gate.release:
	case <-ctx.Done():
		// Kitex's actual contextWithCancelReason.Err exposes the original
		// cancellation exception, including TypeId/path. No expected code is
		// manufactured, and cancellation does not consume queued DATA.
		return ctx.Err()
	}
	if err = ctx.Err(); err != nil {
		return err
	}
	for {
		var value *message.Message
		value, err = stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			err = nil
			break
		}
		if err != nil {
			return err
		}
		received++
		receivedBytes += int64(len(value.Value))
	}
	released, err := lifecycleMessage("released", request.ID)
	if err != nil {
		return err
	}
	released.Seq = received
	return stream.Send(ctx, released)
}

func newBudgetServer(profile Profile, listener net.Listener) (server.Server, error) {
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(service.NewServiceInfo())
	}
	options = append(options, server.WithListener(listener))
	h := &budgetHandler{}
	go h.controls(os.Stdin)
	return service.NewServer(h, options...), nil
}

func budgetTerminal(err error) map[string]any {
	terminal := lifecycleTerminal(err)
	terminal["streaming_canceled"] = errors.Is(err, kerrors.ErrStreamingCanceled)
	return terminal
}

// budgetCall uses the actual generated StreamX client and its single-connection
// mux pool. Raw-wire cases separately prove physical same-socket ordering; this
// fixture observes the public Go error (TypeId and ErrStreamingCanceled).
func budgetCall(profile Profile, addr, pool, identity, report string, count, payloadBytes int, timeout time.Duration, cancelAfterBurst bool) error {
	if identity == "" || pool != "mux" || timeout <= 0 || count < 1 || count > 1<<20 || payloadBytes < 1 || payloadBytes > 1<<20 || uint64(count)*uint64(payloadBytes) > 128<<20 {
		return fmt.Errorf("budget fixture requires id, mux pool, positive timeout and bounded 1..128MiB input")
	}
	c, err := newTypedClient(profile, addr, pool,
		client.WithClientBasicInfo(&rpcinfo.EndpointBasicInfo{ServiceName: "A"}))
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	start := time.Now()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	hold, err := lifecycleMessage("hold", identity)
	if err != nil {
		return err
	}
	if err = stream.Send(ctx, hold); err != nil {
		return err
	}
	ack, err := stream.Recv(ctx)
	if err != nil || !lifecycleAction(ack, "ack", identity) {
		return fmt.Errorf("budget hold acknowledgment differs: %v", err)
	}
	lifecycleEvent(map[string]any{"event": "ack", "id": identity})
	payload := strings.Repeat("x", payloadBytes)
	attempted, sent := 0, 0
	var sendErr error
	for i := 0; i < count; i++ {
		attempted++
		if sendErr = stream.Send(ctx, &message.Message{Value: payload, Seq: int32(i)}); sendErr != nil {
			break
		}
		sent++
	}
	burstElapsed := time.Since(start)
	lifecycleEvent(map[string]any{"event": "burst_sent", "id": identity, "attempted": attempted,
		"sent": sent, "target_messages": count, "payload_bytes": payloadBytes, "elapsed_ns": burstElapsed.Nanoseconds(),
		"send_terminal": budgetTerminal(sendErr)})
	// The new HEADER is submitted only after the burst. The server is still
	// gated; the runner must wait for probe_done before releasing it.
	probeStarted := time.Now()
	probeErr := lifecycleProbe(c, identity+"-probe")
	probeElapsed := time.Since(probeStarted)
	lifecycleEvent(map[string]any{"event": "probe_done", "id": identity, "ok": probeErr == nil,
		"elapsed_ns": probeElapsed.Nanoseconds(), "terminal": budgetTerminal(probeErr)})
	var closeErr error
	if cancelAfterBurst {
		cancel()
		lifecycleEvent(map[string]any{"event": "cancel_sent", "id": identity, "elapsed_ns": time.Since(start).Nanoseconds()})
	} else {
		closeErr = stream.CloseSend(ctx)
	}
	responses := make([]*message.Message, 0, 1)
	var terminalErr error
	for len(responses) < 2 {
		var response *message.Message
		response, terminalErr = stream.Recv(ctx)
		if terminalErr != nil {
			break
		}
		responses = append(responses, response)
	}
	observed := terminalErr != nil && probeErr == nil
	result := map[string]any{"schemaVersion": 1, "event": "result", "api": "budget", "profile": profile.Name,
		"kitex_module": profile.KitexModule, "id": identity, "ack": true, "cancel": cancelAfterBurst,
		"target_messages": count, "payload_bytes": payloadBytes, "send_attempted": attempted, "send_succeeded": sent,
		"send_terminal": budgetTerminal(sendErr), "close_terminal": budgetTerminal(closeErr),
		"terminal": budgetTerminal(terminalErr), "responses": responses, "probe": probeErr == nil,
		"probe_terminal": budgetTerminal(probeErr), "probe_elapsed_ns": probeElapsed.Nanoseconds(),
		"burst_elapsed_ns": burstElapsed.Nanoseconds(), "elapsed_ns": time.Since(start).Nanoseconds(),
		"ok": observed}
	if err = writeJSON(report, result); err != nil {
		return err
	}
	if !observed {
		return fmt.Errorf("budget observation incomplete: terminal=%v probe=%v", terminalErr, probeErr)
	}
	return nil
}
