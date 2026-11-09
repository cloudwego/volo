package oracle

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"github.com/bytedance/gopkg/cloud/metainfo"
	"github.com/cloudwego/kitex/client"
	grpcstatus "github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/status"
	"github.com/cloudwego/kitex/transport"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	api "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
)

func newGRPCClient(profile Profile, address string, extra ...client.Option) (service.Client, error) {
	_, target, err := networkAddress(address)
	if err != nil {
		return nil, err
	}
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(service.NewServiceInfo(), "Interop")
	}
	options = append(options, client.WithTransportProtocol(transport.GRPC), client.WithHostPorts(target))
	options = append(options, extra...)
	return service.NewClient("Interop", options...)
}

// No receive is performed while pending. Cancellation must be delivered by
// the transport, including after the request's END_STREAM has been received.
func (h grpcThriftHandler) grpcPending(ctx context.Context, stream api.Interop_BidiServer, request *parent.Message) error {
	parts := strings.Split(string(request.Payload), "|")
	if len(parts) != 3 || parts[0] != "lifecycle" {
		return fmt.Errorf("bad lifecycle request")
	}
	id, phase := parts[1], parts[2]
	defer responseEvent(map[string]any{"event": "exited", "id": id, "phase": phase, "kind": "handler"})
	deadline, timed := ctx.Deadline()
	remaining := int64(-1)
	if timed {
		remaining = time.Until(deadline).Milliseconds()
	}
	transient, _ := metainfo.GetValue(ctx, "INTEROP_ONCE")
	persistent, _ := metainfo.GetPersistentValue(ctx, "INTEROP_PERSIST")
	if err := responseEvent(map[string]any{"event": "started", "id": id, "phase": phase, "remaining_ms": remaining, "transient": transient, "persistent": persistent}); err != nil {
		return err
	}
	if h.downstream != nil {
		time.Sleep(100 * time.Millisecond)
		downstream, err := h.downstream.Bidi(ctx)
		if err != nil {
			return err
		}
		if err = downstream.Send(ctx, request); err != nil {
			return err
		}
		// Recv remains pending for the pre-Header case and propagates parent cancellation.
		response, err := downstream.Recv(ctx)
		if err != nil {
			return err
		}
		if string(response.Payload) != string(request.Payload) {
			return fmt.Errorf("downstream acknowledgement differs")
		}
		if err = stream.Send(ctx, response); err != nil {
			return err
		}
		_, err = downstream.Recv(ctx)
		return err
	}
	if strings.HasPrefix(phase, "after") {
		if err := stream.Send(ctx, request); err != nil {
			return err
		}
	}
	<-ctx.Done()
	if strings.HasSuffix(phase, "-raw") {
		// Separate raw oracle for Kitex's bare handler-error conversion.
		return ctx.Err()
	}
	// A bare context error is wrapped as a business error and may become
	// Internal on this Kitex version. The cancellation fixture returns the
	// canonical status, so timer scheduling cannot change its expected code.
	return grpcstatus.FromContextError(ctx.Err()).Err()
}

func grpcLifecycleCall(profile Profile, address, scenario, id, report string, budget time.Duration) error {
	c, err := newGRPCClient(profile, address)
	if err != nil {
		return err
	}
	ctx := metainfo.WithValue(context.Background(), "INTEROP_ONCE", "seed")
	ctx = metainfo.WithPersistentValue(ctx, "INTEROP_PERSIST", "seed")
	timed := strings.HasPrefix(scenario, "deadline-")
	var cancel context.CancelFunc
	if timed {
		ctx, cancel = context.WithTimeout(ctx, budget)
	} else {
		ctx, cancel = context.WithCancel(ctx)
	}
	defer cancel()
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	phase := "after"
	if strings.HasSuffix(scenario, "before") {
		phase = "before"
	}
	request := &parent.Message{Payload: []byte("lifecycle|" + id + "|" + phase)}
	if err := stream.Send(ctx, request); err != nil {
		return err
	}
	if phase == "after" {
		response, err := stream.Recv(ctx)
		if err != nil || string(response.Payload) != string(request.Payload) {
			return fmt.Errorf("lifecycle acknowledgement: %v", err)
		}
	}
	if scenario == "half-close-cancel" {
		if err := stream.CloseSend(ctx); err != nil {
			return err
		}
	}
	if err := responseEvent(map[string]any{"event": "armed", "id": id, "phase": phase}); err != nil {
		return err
	}
	if !timed {
		line, err := bufio.NewReader(os.Stdin).ReadString('\n')
		if err != nil || strings.TrimSpace(line) != "cancel" {
			return fmt.Errorf("missing cancellation control: %v", err)
		}
		cancel()
	}
	_, err = stream.Recv(ctx)
	if err == nil || err == io.EOF {
		return fmt.Errorf("expected cancellation failure, got %v", err)
	}
	actualCode := 2
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		actualCode = 4
	} else if errors.Is(ctx.Err(), context.Canceled) {
		actualCode = 1
	}
	expected := 1
	if timed {
		expected = 4
	}
	if actualCode != expected {
		return fmt.Errorf("local context terminal cause differs: %v, RPC error: %w", ctx.Err(), err)
	}
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "PASS", "scenario": scenario, "id": id, "code": actualCode, "status_origin": "local-context", "peer_or_transport_code": int(grpcstatus.Code(err)), "actual_error": err.Error(), "profile": profile.Name})
}
