package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type cascadeRequest struct {
	SchemaVersion int    `json:"schemaVersion"`
	ID            string `json:"id"`
	Action        string `json:"action"`
}

type cascadeServer struct {
	node       string
	downstream service.Client
	active     atomic.Int64
	finishMu   sync.Mutex
}

// Exception does not export its cancellation path. Keep the complete error and
// report the path rendered by the actual Kitex error, rather than inventing one.
func cascadeError(err error) (int32, string, []string) {
	path := []string{}
	if err == nil {
		return 0, "", path
	}
	var typed interface{ TypeId() int32 }
	var code int32
	if errors.As(err, &typed) {
		code = typed.TypeId()
	}
	text := err.Error()
	const prefix = "[canceled path: "
	if start := strings.Index(text, prefix); start >= 0 {
		rest := text[start+len(prefix):]
		if end := strings.IndexByte(rest, ']'); end >= 0 && end != 0 {
			path = strings.Split(rest[:end], " -> ")
		}
	}
	return code, text, path
}

func cascadeEvent(event, node, id string, deadline bool, err error, active int64) {
	code, description, path := cascadeError(err)
	encoded, marshalErr := json.Marshal(map[string]any{
		"event": event, "node": node, "id": id, "has_deadline": deadline,
		"code": code, "error": description, "ttscp": path, "active_handlers": active,
	})
	if marshalErr != nil {
		panic(marshalErr)
	}
	fmt.Println(string(encoded))
}

func newCascadeServer(profile Profile, fixture serverFixture) (*cascadeServer, error) {
	h := &cascadeServer{node: fixture.Node}
	if fixture.Forward != "" {
		var err error
		h.downstream, err = newTypedClient(profile, fixture.Forward, profile.DefaultPool,
			client.WithClientBasicInfo(&rpcinfo.EndpointBasicInfo{ServiceName: fixture.Node}))
		if err != nil {
			return nil, err
		}
	}
	return h, nil
}

func (h *cascadeServer) bidi(ctx context.Context, stream message.Interop_BidiServer) (err error) {
	first, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	var request cascadeRequest
	if err = json.Unmarshal([]byte(first.Value), &request); err != nil {
		return err
	}
	if request.SchemaVersion != 1 || request.ID == "" || (request.Action != "cancel" && request.Action != "probe") {
		return fmt.Errorf("invalid cascade request")
	}
	_, hasDeadline := ctx.Deadline()
	h.active.Add(1)
	defer func() {
		// Emit snapshots in decrement order so concurrent handler completion
		// cannot print an older nonzero count after the final zero.
		h.finishMu.Lock()
		defer h.finishMu.Unlock()
		cascadeEvent("cascade_finish", h.node, request.ID, hasDeadline, err, h.active.Add(-1))
	}()
	var child service.Interop_BidiClient
	if h.downstream != nil {
		// Use the unmodified framework handler context. There is deliberately no
		// custom cancellation goroutine or manually linked child context here.
		child, err = h.downstream.Bidi(ctx)
		if err != nil {
			return err
		}
		defer func() {
			cascadeEvent("cascade_child_finish", h.node, request.ID, hasDeadline, err, h.active.Load())
		}()
		if err = child.Send(ctx, first); err != nil {
			return err
		}
		var response *message.Message
		response, err = child.Recv(ctx)
		if err != nil {
			return err
		}
		if *response != *first {
			return fmt.Errorf("cascade child acknowledgment differs")
		}
	}
	if err = stream.Send(ctx, first); err != nil {
		return err
	}
	if request.Action == "probe" {
		if child != nil {
			if err = child.CloseSend(ctx); err != nil {
				return err
			}
			if _, end := child.Recv(ctx); !errors.Is(end, io.EOF) {
				return fmt.Errorf("probe child did not finish: %v", end)
			}
		}
		return nil
	}
	// Keep both sending directions open after the acknowledgment. Completion
	// must come from actual RST cancellation, never an early request Trailer.
	if child != nil {
		_, err = child.Recv(ctx)
	} else {
		_, err = stream.Recv(ctx)
	}
	if err == nil || errors.Is(err, io.EOF) {
		return fmt.Errorf("cascade ended without cancellation: %v", err)
	}
	return err
}

func cascadeCall(profile Profile, addr, pool, mode, identity, report string) error {
	if mode != "cancel" && mode != "deadline" {
		return fmt.Errorf("invalid cascade mode %q", mode)
	}
	c, err := newTypedClient(profile, addr, pool,
		client.WithClientBasicInfo(&rpcinfo.EndpointBasicInfo{ServiceName: "A"}))
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	if mode == "deadline" {
		cancel()
		ctx, cancel = context.WithTimeout(context.Background(), 3*time.Second)
	}
	defer cancel()
	start := time.Now()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(cascadeRequest{SchemaVersion: 1, ID: identity, Action: "cancel"})
	if err != nil {
		return err
	}
	request := &message.Message{Value: string(payload), Seq: 1}
	if err = stream.Send(ctx, request); err != nil {
		return err
	}
	ack, err := stream.Recv(ctx)
	if err != nil || *ack != *request {
		return fmt.Errorf("missing cascade acknowledgment: %v", err)
	}
	if mode == "cancel" {
		cancel()
	}
	_, terminal := stream.Recv(ctx)
	code, description, path := cascadeError(terminal)
	expected := int32(12007)
	if mode == "deadline" {
		expected = 12015
	}
	result := map[string]any{"event": "result", "api": "cascade", "profile": profile.Name,
		"kitex_module": profile.KitexModule, "id": identity, "mode": mode, "ack": true,
		"code": code, "error": description, "ttscp": path, "elapsed_ms": time.Since(start).Milliseconds()}
	probeErr := cascadeProbe(c, identity+"-probe")
	result["probe_ok"] = probeErr == nil
	result["ok"] = code == expected && probeErr == nil
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	if probeErr != nil {
		return probeErr
	}
	if code != expected {
		return fmt.Errorf("cascade terminal code=%d expected=%d: %v", code, expected, terminal)
	}
	return nil
}

func cascadeProbe(c service.Client, identity string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	payload, err := json.Marshal(cascadeRequest{SchemaVersion: 1, ID: identity, Action: "probe"})
	if err != nil {
		return err
	}
	request := &message.Message{Value: string(payload), Seq: 1}
	if err = stream.Send(ctx, request); err != nil {
		return err
	}
	response, err := stream.Recv(ctx)
	if err != nil || *response != *request {
		return fmt.Errorf("cascade recovery acknowledgment differs: %v", err)
	}
	if err = stream.CloseSend(ctx); err != nil {
		return err
	}
	if _, end := stream.Recv(ctx); !errors.Is(end, io.EOF) {
		return fmt.Errorf("cascade recovery did not finish: %v", end)
	}
	return nil
}
