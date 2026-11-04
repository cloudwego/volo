package oracle

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"sync/atomic"
	"time"

	"github.com/cloudwego/kitex/server"
	"github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/audit"
	"github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/audit/child"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/audit_parent"
)

type inheritedHandler struct{ calls atomic.Uint64 }

func (h *inheritedHandler) called(method string) {
	_ = responseEvent(map[string]any{"event": "handler", "method": method, "call_index": h.calls.Add(1)})
}
func (h *inheritedHandler) Plain(_ context.Context, req *parent.Request) (*parent.Response, error) {
	h.called("Plain")
	return &parent.Response{Value: 10000 + req.Value}, nil
}
func (h *inheritedHandler) Unary(_ context.Context, req *parent.Request) (*parent.Response, error) {
	h.called("Unary")
	return &parent.Response{Value: 20000 + req.Value}, nil
}
func (h *inheritedHandler) Upload(ctx context.Context, stream parent.Parent_UploadServer) error {
	h.called("Upload")
	var count int32
	for {
		req, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			return stream.SendAndClose(ctx, &parent.Response{Value: 30000 + count})
		}
		if err != nil {
			return err
		}
		if req.Value != count {
			return fmt.Errorf("Upload request sequence differs")
		}
		count++
	}
}
func (h *inheritedHandler) Watch(ctx context.Context, req *parent.Request, stream parent.Parent_WatchServer) error {
	h.called("Watch")
	for i := int32(0); i < req.Value; i++ {
		if err := stream.Send(ctx, &parent.Response{Value: 40000 + i}); err != nil {
			return err
		}
	}
	return nil
}
func (h *inheritedHandler) Chat(ctx context.Context, stream audit.Child_ChatServer) error {
	h.called("Chat")
	for {
		req, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if err = stream.Send(ctx, &audit.RESULT{Value: 50000 + req.Value}); err != nil {
			return err
		}
	}
}
func (h *inheritedHandler) WatchResult_(ctx context.Context, req *audit.RESULT, stream audit.Child_WatchResultServer) error {
	h.called("WatchResult")
	for i := int32(0); i < req.Value; i++ {
		if err := stream.Send(ctx, &audit.RESULT{Value: 60000 + i}); err != nil {
			return err
		}
	}
	return nil
}

func serveInherited(profile Profile, addr string) error {
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	listener, err := net.Listen(network, target)
	if err != nil {
		return err
	}
	defer listener.Close()
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(child.NewServiceInfo())
	}
	svr := child.NewServer(&inheritedHandler{}, append(options, server.WithListener(listener))...)
	address := "tcp://" + listener.Addr().String()
	if network == "unix" {
		address = "unix://" + target
	}
	if err = responseEvent(map[string]any{"event": "ready", "api": "inherited", "service": "Child", "addr": address,
		"profile": profile.Name, "kitex_module": profile.KitexModule}); err != nil {
		return err
	}
	return svr.Run()
}

func inheritedCall(ctx context.Context, c child.Client, method string, count int) ([]int32, error) {
	responses := make([]int32, 0)
	switch method {
	case "Plain", "Unary":
		var response *parent.Response
		var err error
		if method == "Plain" {
			response, err = c.Plain(ctx, &parent.Request{Value: int32(count)})
		} else {
			response, err = c.Unary(ctx, &parent.Request{Value: int32(count)})
		}
		if err != nil {
			return nil, err
		}
		responses = append(responses, response.Value)
	case "Upload":
		stream, err := c.Upload(ctx)
		if err != nil {
			return nil, err
		}
		for i := 0; i < count; i++ {
			if err = stream.Send(ctx, &parent.Alias{Value: int32(i)}); err != nil {
				return nil, err
			}
		}
		response, err := stream.CloseAndRecv(ctx)
		if err != nil {
			return nil, err
		}
		responses = append(responses, response.Value)
	case "Watch":
		stream, err := c.Watch(ctx, &parent.Request{Value: int32(count)})
		if err != nil {
			return nil, err
		}
		for {
			response, err := stream.Recv(ctx)
			if errors.Is(err, io.EOF) {
				break
			}
			if err != nil {
				return nil, err
			}
			responses = append(responses, response.Value)
		}
	case "WatchResult":
		stream, err := c.WatchResult_(ctx, &audit.RESULT{Value: int32(count)})
		if err != nil {
			return nil, err
		}
		for {
			response, err := stream.Recv(ctx)
			if errors.Is(err, io.EOF) {
				break
			}
			if err != nil {
				return nil, err
			}
			responses = append(responses, response.Value)
		}
	case "Chat":
		stream, err := c.Chat(ctx)
		if err != nil {
			return nil, err
		}
		for i := 0; i < count; i++ {
			if err = stream.Send(ctx, &audit.RESULT{Value: int32(i)}); err != nil {
				return nil, err
			}
		}
		if err = stream.CloseSend(ctx); err != nil {
			return nil, err
		}
		for {
			response, err := stream.Recv(ctx)
			if errors.Is(err, io.EOF) {
				break
			}
			if err != nil {
				return nil, err
			}
			responses = append(responses, response.Value)
		}
	default:
		return nil, fmt.Errorf("unknown inherited method %q", method)
	}
	return responses, nil
}

func callInherited(profile Profile, addr, method, pool, report string, count int, timeout time.Duration) error {
	if count > 4096 {
		return fmt.Errorf("inherited fixture message bound exceeded")
	}
	options, err := newClientOptionsForService(profile, addr, pool, child.NewServiceInfo(), "Child")
	if err != nil {
		return err
	}
	c, err := child.NewClient("Child", options...)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	methods := []string{method}
	if method == "all" {
		methods = []string{"Plain", "Unary", "Upload", "Watch", "Chat", "WatchResult"}
	}
	calls := make([]map[string]any, 0, len(methods))
	for _, name := range methods {
		responses, err := inheritedCall(ctx, c, name, count)
		if err != nil {
			return fmt.Errorf("%s: %w", name, err)
		}
		calls = append(calls, map[string]any{"method": name, "responses": responses})
	}
	return writeJSON(report, map[string]any{"event": "result", "api": "inherited", "service": "Child",
		"method": method, "ok": true, "calls": calls, "profile": profile.Name})
}
