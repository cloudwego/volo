package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	"github.com/cloudwego/kitex/pkg/serviceinfo"
	"github.com/cloudwego/kitex/server"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type handler struct {
	metadata     *metadataServer
	cascade      *cascadeServer
	errors       bool
	errorForward *errorForwardServer
}

func (handler) Unary(_ context.Context, req *message.Message) (*message.Message, error) {
	return req, nil
}
func (handler) ExplicitUnary(_ context.Context, req *message.Message) (*message.Message, error) {
	return req, nil
}
func (handler) Client(ctx context.Context, stream message.Interop_ClientServer) error {
	last, count := "", int32(0)
	for {
		m, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return err
		}
		last = m.Value
		count++
	}
	return stream.SendAndClose(ctx, &message.Message{Value: last, Seq: count})
}
func (handler) Server(ctx context.Context, req *message.Message, stream message.Interop_ServerServer) error {
	for i := int32(0); i < req.Seq; i++ {
		if err := stream.Send(ctx, &message.Message{Value: req.Value, Seq: i}); err != nil {
			return err
		}
	}
	return nil
}
func (h handler) Bidi(ctx context.Context, stream message.Interop_BidiServer) error {
	if h.errorForward != nil {
		return h.errorForward.bidi(ctx, stream)
	}
	if h.errors {
		return errorStatusBidi(ctx, stream)
	}
	if h.cascade != nil {
		return h.cascade.bidi(ctx, stream)
	}
	if h.metadata != nil {
		return h.metadata.bidi(ctx, stream)
	}
	for {
		m, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if err = stream.Send(ctx, m); err != nil {
			return err
		}
	}
}

func networkAddress(addr string) (string, string, error) {
	if strings.HasPrefix(addr, "tcp://") {
		return "tcp", strings.TrimPrefix(addr, "tcp://"), nil
	}
	if strings.HasPrefix(addr, "unix://") {
		return "unix", strings.TrimPrefix(addr, "unix://"), nil
	}
	return "", "", fmt.Errorf("address must start tcp:// or unix://")
}

func serve(profile Profile, addr string, fixture serverFixture) error {
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	listener, err := net.Listen(network, target)
	if err != nil {
		return err
	}
	defer listener.Close()
	actual := network + "://" + listener.Addr().String()
	server.RegisterStartHook(func() {
		b, _ := json.Marshal(map[string]any{"event": "ready", "addr": actual, "pid": os.Getpid(), "profile": profile.Name, "kitex_module": profile.KitexModule,
			"scenario": fixture.Scenario, "node": fixture.Node, "metainfo_mode": fixture.MetainfoMode, "api": fixture.API, "routing_profile": fixture.RoutingProfile})
		fmt.Println(string(b))
	})
	if fixture.API == "binary" {
		if fixture.Scenario != "echo" {
			return fmt.Errorf("binary generic server fixture only supports echo")
		}
		svr, err := newBinaryGenericServer(profile, listener)
		if err != nil {
			return err
		}
		return svr.Run()
	}
	if fixture.Scenario == "routing" {
		svr, err := newRoutingServer(profile, listener, fixture.RoutingProfile)
		if err != nil {
			return err
		}
		return svr.Run()
	}
	if fixture.Scenario == "budget" {
		svr, err := newBudgetServer(profile, listener)
		if err != nil {
			return err
		}
		return svr.Run()
	}
	if fixture.Scenario == "keep-open" || fixture.Scenario == "cancel" || fixture.Scenario == "disconnect" {
		svr, err := newLifecycleServer(profile, listener, fixture.Scenario)
		if err != nil {
			return err
		}
		return svr.Run()
	}
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(service.NewServiceInfo())
	}
	options = append(options, server.WithListener(listener))
	h := &handler{}
	if fixture.Scenario == "metainfo" {
		h.metadata, err = newMetadataServer(profile, fixture)
		if err != nil {
			return err
		}
	} else if fixture.Scenario == "cascade" {
		h.cascade, err = newCascadeServer(profile, fixture)
		if err != nil {
			return err
		}
	} else if fixture.Scenario == "errors" {
		h.errors = true
		if fixture.Forward != "" {
			h.errorForward, err = newErrorForwardServer(profile, fixture)
			if err != nil {
				return err
			}
		}
	} else if fixture.Scenario != "echo" {
		return fmt.Errorf("unsupported server scenario %q", fixture.Scenario)
	}
	svr := service.NewServer(h, options...)
	if err = setMetainfoFixtureMode(fixture.MetainfoMode); err != nil {
		return err
	}
	return svr.Run()
}

func newTypedClient(profile Profile, addr, pool string, extra ...client.Option) (service.Client, error) {
	options, err := newClientOptions(profile, addr, pool, service.NewServiceInfo())
	if err != nil {
		return nil, err
	}
	options = append(options, extra...)
	return service.NewClient("Interop", options...)
}

func newClientOptions(profile Profile, addr, pool string, info *serviceinfo.ServiceInfo) ([]client.Option, error) {
	return newClientOptionsForService(profile, addr, pool, info, "Interop")
}

func newClientOptionsForService(profile Profile, addr, pool string, info *serviceinfo.ServiceInfo, name string) ([]client.Option, error) {
	_, target, err := networkAddress(addr)
	if err != nil {
		return nil, err
	}
	var poolOpt ttstream.ClientHandlerOption
	switch pool {
	case "short":
		poolOpt = ttstream.WithClientShortConnPool()
	case "long":
		poolOpt = ttstream.WithClientLongConnPool(ttstream.DefaultLongConnConfig)
	case "mux":
		poolOpt = ttstream.WithClientMuxConnPool(ttstream.MuxConnConfig{PoolSize: 1, MaxIdleTimeout: time.Minute})
	default:
		return nil, fmt.Errorf("invalid pool %q", pool)
	}
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(info, name)
	}
	options = append(options, client.WithHostPorts(target),
		client.WithTTHeaderStreamingOptions(client.WithTTHeaderStreamingTransportOptions(poolOpt)))
	return options, nil
}

func call(profile Profile, addr, mode, payload, pool, report string, count int, timeout time.Duration) error {
	c, err := newTypedClient(profile, addr, pool)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	responses := make([]*message.Message, 0)
	start := time.Now()
	switch mode {
	case "unary", "explicit-unary":
		var m *message.Message
		request := &message.Message{Value: payload, Seq: int32(count)}
		if mode == "unary" {
			m, err = c.Unary(ctx, request)
		} else {
			m, err = c.ExplicitUnary(ctx, request)
		}
		if err == nil {
			responses = append(responses, m)
		}
	case "client":
		var stream service.Interop_ClientClient
		stream, err = c.Client(ctx)
		if err != nil {
			break
		}
		for i := 0; i < count; i++ {
			err = stream.Send(ctx, &message.Message{Value: payload, Seq: int32(i)})
			if err != nil {
				break
			}
		}
		if err == nil {
			var m *message.Message
			m, err = stream.CloseAndRecv(ctx)
			if err == nil {
				responses = append(responses, m)
			}
		}
	case "server":
		var stream service.Interop_ServerClient
		stream, err = c.Server(ctx, &message.Message{Value: payload, Seq: int32(count)})
		if err != nil {
			break
		}
		for {
			m, recvErr := stream.Recv(ctx)
			if errors.Is(recvErr, io.EOF) {
				break
			}
			if recvErr != nil {
				err = recvErr
				break
			}
			responses = append(responses, m)
		}
	case "bidi":
		var stream service.Interop_BidiClient
		stream, err = c.Bidi(ctx)
		if err != nil {
			break
		}
		// Concurrent send/recv also validates that the implementation supports full duplex.
		sent := make(chan error, 1)
		go func() {
			for i := 0; i < count; i++ {
				if e := stream.Send(ctx, &message.Message{Value: payload, Seq: int32(i)}); e != nil {
					sent <- e
					return
				}
			}
			sent <- stream.CloseSend(ctx)
		}()
		for {
			m, recvErr := stream.Recv(ctx)
			if errors.Is(recvErr, io.EOF) {
				break
			}
			if recvErr != nil {
				err = recvErr
				break
			}
			responses = append(responses, m)
		}
		select {
		case sendErr := <-sent:
			if err == nil {
				err = sendErr
			}
		case <-ctx.Done():
			if err == nil {
				err = ctx.Err()
			}
		}
	default:
		err = fmt.Errorf("invalid mode %q", mode)
	}
	result := map[string]any{"event": "result", "schemaVersion": 1, "ok": err == nil, "mode": mode, "pool": pool, "responses": responses, "elapsed_ns": time.Since(start).Nanoseconds(), "status": "ok"}
	result["profile"] = profile.Name
	result["kitex_module"] = profile.KitexModule
	if err != nil {
		result["status"] = "error"
		result["error"] = err.Error()
		var typed interface{ TypeId() int32 }
		if errors.As(err, &typed) {
			result["type_id"] = typed.TypeId()
		}
	}
	if outputErr := writeJSON(report, result); outputErr != nil {
		return outputErr
	}
	return err
}
