package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"sync/atomic"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	"github.com/cloudwego/kitex/pkg/streaming"
	"github.com/cloudwego/kitex/server"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type lifecycleRequest struct {
	SchemaVersion int    `json:"schemaVersion"`
	Action        string `json:"action"`
	ID            string `json:"id"`
}

func lifecycleMessage(action, id string) (*message.Message, error) {
	value, err := json.Marshal(lifecycleRequest{SchemaVersion: 1, Action: action, ID: id})
	if err != nil {
		return nil, err
	}
	return &message.Message{Value: string(value), Seq: 1}, nil
}
func lifecycleAction(value *message.Message, action, id string) bool {
	if value == nil || value.Seq != 1 {
		return false
	}
	var parsed lifecycleRequest
	return json.Unmarshal([]byte(value.Value), &parsed) == nil && parsed.SchemaVersion == 1 && parsed.Action == action && parsed.ID == id
}
func lifecycleTerminal(err error) map[string]interface{} {
	if err == nil || errors.Is(err, io.EOF) {
		return map[string]interface{}{"kind": "ok", "code": int32(0), "kitex_code": int32(0), "path": []string{}, "error": nil}
	}
	code, text, path := cascadeError(err)
	return map[string]interface{}{"kind": "error", "code": code, "kitex_code": code, "path": path, "error": text}
}
func lifecycleEvent(value interface{}) {
	line, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	fmt.Println(string(line))
}

type lifecycleHandler struct {
	handler
	active atomic.Int64
}

func (h *lifecycleHandler) Bidi(ctx context.Context, stream message.Interop_BidiServer) (err error) {
	first, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	var request lifecycleRequest
	if err = json.Unmarshal([]byte(first.Value), &request); err != nil {
		return err
	}
	if first.Seq != 1 || request.SchemaVersion != 1 || request.ID == "" || (request.Action != "start" && request.Action != "probe") {
		return fmt.Errorf("invalid lifecycle request")
	}
	if request.Action == "probe" {
		return stream.Send(ctx, first)
	}
	h.active.Add(1)
	defer func() {
		lifecycleEvent(map[string]interface{}{"event": "observed", "id": request.ID, "terminal": lifecycleTerminal(err), "active_handlers": h.active.Add(-1)})
	}()
	var connection interface{}
	if info := rpcinfo.GetRPCInfo(ctx); info != nil && info.From().Address() != nil {
		connection = info.From().Address().String()
	}
	lifecycleEvent(map[string]interface{}{"event": "started", "id": request.ID, "connection": connection})
	ack, err := lifecycleMessage("ack", request.ID)
	if err != nil {
		return err
	}
	if err = stream.Send(ctx, ack); err != nil {
		return err
	}
	next, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	if !lifecycleAction(next, "release", request.ID) {
		return fmt.Errorf("invalid lifecycle release")
	}
	released, err := lifecycleMessage("released", request.ID)
	if err != nil {
		return err
	}
	return stream.Send(ctx, released)
}
func newLifecycleServer(profile Profile, listener net.Listener, scenario string) (server.Server, error) {
	if scenario != "keep-open" && scenario != "cancel" && scenario != "disconnect" {
		return nil, fmt.Errorf("Go lifecycle server unsupported scenario %q", scenario)
	}
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(service.NewServiceInfo())
	}
	options = append(options, server.WithListener(listener))
	return service.NewServer(&lifecycleHandler{}, options...), nil
}
func lifecycleProbe(c service.Client, id string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	request, err := lifecycleMessage("probe", id)
	if err != nil {
		return err
	}
	if err = stream.Send(ctx, request); err != nil {
		return err
	}
	response, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	if !lifecycleAction(response, "probe", id) {
		return fmt.Errorf("lifecycle probe identity differs")
	}
	if err = stream.CloseSend(ctx); err != nil {
		return err
	}
	if _, err = stream.Recv(ctx); !errors.Is(err, io.EOF) {
		return fmt.Errorf("lifecycle probe missing EOF: %v", err)
	}
	return nil
}

// Timeout is the actual Kitex per-Recv configuration. No per-call wrapper timer
// is used to simulate 12014; the outer context only bounds the fixture's lifetime.
func lifecycleCall(profile Profile, addr, pool, scenario, id, report string, timeout time.Duration) error {
	if timeout <= 0 {
		return fmt.Errorf("lifecycle receive timeout must be positive")
	}
	if scenario != "keep-open" && scenario != "cancel" && scenario != "server-timeout" && scenario != "disconnect" {
		return fmt.Errorf("Go lifecycle client unsupported scenario %q", scenario)
	}
	options := []client.Option{client.WithClientBasicInfo(&rpcinfo.EndpointBasicInfo{ServiceName: "A"})}
	if scenario == "keep-open" || scenario == "cancel" {
		options = append(options, client.WithStreamOptions(client.WithStreamRecvTimeoutConfig(streaming.TimeoutConfig{Timeout: timeout, DisableCancelRemote: scenario == "keep-open"})))
	}
	c, err := newTypedClient(profile, addr, pool, options...)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	started := time.Now()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	request, err := lifecycleMessage("start", id)
	if err != nil {
		return err
	}
	if err = stream.Send(ctx, request); err != nil {
		return err
	}
	ack, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	if !lifecycleAction(ack, "ack", id) {
		return fmt.Errorf("lifecycle acknowledgment differs")
	}
	responses := []map[string]interface{}{{"value": ack.Value, "seq": ack.Seq}}
	lifecycleEvent(map[string]interface{}{"event": "ack", "id": id})
	_, terminalErr := stream.Recv(ctx)
	terminal := lifecycleTerminal(terminalErr)
	var finalTerminal interface{}
	keepOpen := false
	expected := int32(12014)
	if scenario == "server-timeout" {
		expected = 12015
	} else if scenario == "disconnect" {
		// This actual Kitex/netpoll client read path wraps netpoll.ErrEOF in
		// DecodeFrame as 12004; serverTransport separately maps closure to 12013.
		expected = 12004
	}
	if scenario == "keep-open" {
		release, makeErr := lifecycleMessage("release", id)
		if makeErr != nil {
			return makeErr
		}
		err = stream.Send(ctx, release)
		if err == nil {
			err = stream.CloseSend(ctx)
		}
		var released *message.Message
		if err == nil {
			released, err = stream.Recv(ctx)
		}
		if released != nil {
			responses = append(responses, map[string]interface{}{"value": released.Value, "seq": released.Seq})
		}
		if err == nil && lifecycleAction(released, "released", id) {
			_, end := stream.Recv(ctx)
			finalTerminal = lifecycleTerminal(end)
			keepOpen = errors.Is(end, io.EOF)
		} else {
			finalTerminal = lifecycleTerminal(err)
		}
	}
	probe := false
	var probeTerminal interface{}
	if scenario != "disconnect" {
		probeErr := lifecycleProbe(c, id+"-probe")
		probe = probeErr == nil
		probeTerminal = lifecycleTerminal(probeErr)
	}
	ok := terminal["code"] == expected && (scenario == "disconnect" || probe) && (scenario != "keep-open" || keepOpen)
	result := map[string]interface{}{"event": "result", "api": "lifecycle", "profile": profile.Name, "scenario": scenario, "id": id, "ack": true, "terminal": terminal, "final_terminal": finalTerminal, "keep_open": keepOpen, "responses": responses, "probe": probe, "probe_terminal": probeTerminal, "elapsed_ms": time.Since(started).Milliseconds(), "ok": ok}
	if err = writeJSON(report, result); err != nil {
		return err
	}
	if !ok {
		return fmt.Errorf("lifecycle observation did not meet scenario: %s", scenario)
	}
	return nil
}
