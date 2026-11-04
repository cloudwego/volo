package oracle

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"time"

	"github.com/cloudwego/kitex/pkg/generic"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	"github.com/cloudwego/kitex/pkg/serviceinfo"
	"github.com/cloudwego/kitex/server"
	"github.com/cloudwego/kitex/server/genericserver"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

// Reuse the actual generated adapters. Only names/method tables vary; no alternate
// SearchService implementation is introduced by this fixture.
func routingInfo(name string, streamingMethods, unaryMethods []string) *serviceinfo.ServiceInfo {
	base := service.NewServiceInfo()
	result := *base
	result.ServiceName = name
	result.Methods = make(map[string]serviceinfo.MethodInfo)
	result.Extra = map[string]interface{}{"PackageName": "interop"}
	for _, method := range streamingMethods {
		result.Methods[method] = base.Methods["Bidi"]
	}
	for _, method := range unaryMethods {
		result.Methods[method] = base.Methods["Unary"]
	}
	return &result
}

type routingHandler struct {
	handler
	selected string
}

func routingReply(ctx context.Context, selected string, request *message.Message) (*message.Message, error) {
	info := rpcinfo.GetRPCInfo(ctx)
	if info == nil {
		return nil, fmt.Errorf("routing fixture missing RPCInfo")
	}
	value, err := json.Marshal(map[string]interface{}{"schemaVersion": 1, "handler": selected, "service": info.Invocation().ServiceName(), "method": info.Invocation().MethodName(), "request": request.Value})
	if err != nil {
		return nil, err
	}
	return &message.Message{Value: string(value), Seq: request.Seq}, nil
}
func (h *routingHandler) Bidi(ctx context.Context, stream message.Interop_BidiServer) error {
	request, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	response, err := routingReply(ctx, h.selected, request)
	if err != nil {
		return err
	}
	return stream.Send(ctx, response)
}
func (h *routingHandler) Unary(ctx context.Context, request *message.Message) (*message.Message, error) {
	return routingReply(ctx, h.selected, request)
}
func routingGeneric(selected string) func(context.Context, string, string, generic.BidiStreamingServer) error {
	return func(ctx context.Context, _, _ string, stream generic.BidiStreamingServer) error {
		value, err := stream.Recv(ctx)
		if err != nil {
			return err
		}
		request, err := binaryGenericMessage(value)
		if err != nil {
			return err
		}
		response, err := routingReply(ctx, selected, request)
		if err != nil {
			return err
		}
		return stream.Send(ctx, encoded(response))
	}
}

type routingLegacy struct{}

func (*routingLegacy) GenericCall(context.Context, string, interface{}) (interface{}, error) {
	return nil, fmt.Errorf("registration-only legacy fixture must not receive calls")
}

func routingUnknown(svr server.Server) error {
	return genericserver.RegisterUnknownServiceOrMethodHandler(svr, &genericserver.UnknownServiceOrMethodHandler{StreamingHandler: routingGeneric("unknown")})
}
func routingRegister(svr server.Server, name string, streamingMethods, unaryMethods []string, fallback bool) error {
	var options []server.RegisterOption
	if fallback {
		options = append(options, server.WithFallbackService())
	}
	return svr.RegisterService(routingInfo(name, streamingMethods, unaryMethods), &routingHandler{selected: name}, options...)
}

// newRoutingServer supports successful public registration configurations.
// A miss without unknown fallback intentionally retains Go's no-response behavior;
// the common runner observes that separately from Rust's UnknownMethod enhancement.
func newRoutingServer(profile Profile, listener net.Listener, mode string) (server.Server, error) {
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(routingInfo("A", []string{"OnlyA"}, nil))
	}
	options = append(options, server.WithListener(listener))
	if mode == "strict" {
		options = append(options, server.WithRefuseTrafficWithoutServiceName())
	}
	svr := server.NewServer(options...)
	register := func(name string, methods, unary []string, fallback bool) error {
		return routingRegister(svr, name, methods, unary, fallback)
	}
	switch mode {
	case "unique", "unknown", "dynamic", "binary-v2":
		if err := register("A", []string{"OnlyA"}, []string{"UnaryA"}, false); err != nil {
			return nil, err
		}
		if mode == "dynamic" {
			info := routingInfo("B", nil, nil)
			info.GenericMethod = func(_ context.Context, method string) serviceinfo.MethodInfo {
				if method == "OnlyB" {
					return service.NewServiceInfo().Methods["Bidi"]
				}
				return nil
			}
			if err := svr.RegisterService(info, &routingHandler{selected: "B"}); err != nil {
				return nil, err
			}
		} else if mode == "binary-v2" {
			g := generic.BinaryThriftGenericV2("B")
			if err := genericserver.RegisterService(svr, &generic.ServiceV2{BidiStreaming: routingGeneric("B")}, g); err != nil {
				return nil, err
			}
		} else if err := register("B", []string{"OnlyB"}, nil, false); err != nil {
			return nil, err
		}
		if mode == "unknown" {
			if err := routingUnknown(svr); err != nil {
				return nil, err
			}
		}
	case "fallback", "strict":
		if err := register("A", []string{"OnlyA", "Shared"}, nil, mode == "fallback"); err != nil {
			return nil, err
		}
		if err := register("B", []string{"OnlyB", "Shared"}, nil, false); err != nil {
			return nil, err
		}
	case "mixed":
		if err := register("A", []string{"OnlyA"}, []string{"Shared"}, true); err != nil {
			return nil, err
		}
		if err := register("B", []string{"OnlyB", "Shared"}, nil, false); err != nil {
			return nil, err
		}
	case "combine":
		info := routingInfo("Combined", []string{"OnlyA", "OnlyB", "Shared"}, nil)
		info.Extra[serviceinfo.CombineServiceKey] = true
		if err := svr.RegisterService(info, &routingHandler{selected: "Combined"}); err != nil {
			return nil, err
		}
	default:
		return nil, fmt.Errorf("unsupported routing profile %q", mode)
	}
	return svr, nil
}

// Actual public registration may panic, whereas Run returns configuration errors.
// Preserve that distinction in observations instead of pretending both are Results.
func routingRegistrationCase(profile Profile, mode string) (result map[string]interface{}) {
	result = map[string]interface{}{"name": mode, "stage": "register", "outcome": "unexpected-success"}
	defer func() {
		if value := recover(); value != nil {
			result["outcome"] = "panic"
			result["error"] = fmt.Sprint(value)
		}
	}()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		result["outcome"] = "environment-error"
		result["error"] = err.Error()
		return
	}
	defer listener.Close()
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(routingInfo("A", []string{"OnlyA"}, nil))
	}
	options = append(options, server.WithListener(listener))
	svr := server.NewServer(options...)
	add := func(name string, methods []string, fallback bool) {
		if err := routingRegister(svr, name, methods, nil, fallback); err != nil {
			panic(err)
		}
	}
	addUnknown := func() {
		if err := routingUnknown(svr); err != nil {
			panic(err)
		}
	}
	addCombine := func() {
		info := routingInfo("Combined", []string{"Shared"}, nil)
		info.Extra[serviceinfo.CombineServiceKey] = true
		if err := svr.RegisterService(info, &routingHandler{selected: "Combined"}); err != nil {
			panic(err)
		}
	}
	addV1 := func() {
		// Legacy v1 has its own public constructor; the V2 register helper rejects it.
		svr = genericserver.NewServer(&routingLegacy{}, generic.BinaryThriftGeneric(), options...)
	}
	switch mode {
	case "duplicate-service":
		add("A", []string{"OnlyA"}, false)
		add("A", []string{"OnlyB"}, false)
	case "duplicate-fallback":
		add("A", []string{"OnlyA"}, true)
		add("B", []string{"OnlyB"}, true)
	case "duplicate-unknown":
		addUnknown()
		addUnknown()
	case "conflicting-methods":
		add("A", []string{"Shared"}, false)
		add("B", []string{"Shared"}, false)
	case "fallback-missing-conflict":
		add("A", []string{"OnlyA"}, true)
		add("B", []string{"Shared"}, false)
		add("C", []string{"Shared"}, false)
	case "combine-multi":
		addCombine()
		add("A", []string{"OnlyA"}, false)
	case "combine-unknown":
		addCombine()
		addUnknown()
	case "binary-v1-multi":
		addV1()
		add("A", []string{"OnlyA"}, false)
	case "binary-v1-unknown":
		addV1()
		addUnknown()
	default:
		result["outcome"] = "fixture-error"
		result["error"] = "unknown registration profile"
		return
	}
	result["stage"] = "run"
	done := make(chan error, 1)
	go func() {
		defer func() {
			if p := recover(); p != nil {
				done <- fmt.Errorf("Run panic: %v", p)
			}
		}()
		done <- svr.Run()
	}()
	select {
	case err := <-done:
		if err != nil {
			result["outcome"] = "error"
			result["error"] = err.Error()
		}
	case <-time.After(2 * time.Second):
		result["outcome"] = "timeout"
		result["error"] = "invalid registration unexpectedly entered serving loop"
		_ = svr.Stop()
	}
	return
}
func routingRegistration(profile Profile, report string) error {
	cases := []map[string]interface{}{}
	ok := true
	for _, mode := range []string{"duplicate-service", "duplicate-fallback", "duplicate-unknown", "conflicting-methods", "fallback-missing-conflict", "combine-multi", "combine-unknown", "binary-v1-multi", "binary-v1-unknown"} {
		observed := routingRegistrationCase(profile, mode)
		cases = append(cases, observed)
		outcome := observed["outcome"]
		if outcome != "error" && outcome != "panic" {
			ok = false
		}
	}
	return writeJSON(report, map[string]interface{}{"event": "result", "api": "routing-registration", "profile": profile.Name, "ok": ok, "cases": cases})
}
