package oracle

import (
	"context"
	"errors"
	"fmt"
	"io"
	"math"
	"net"
	"reflect"
	"strings"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/client/callopt"
	"github.com/cloudwego/kitex/client/callopt/streamcall"
	"github.com/cloudwego/kitex/pkg/kerrors"
	"github.com/cloudwego/kitex/pkg/remote"
	"github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/metadata"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	"github.com/cloudwego/kitex/pkg/streaming"
	"github.com/cloudwego/kitex/server"
	"github.com/cloudwego/kitex/transport"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	api "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
	oldservice "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api/interop"
)

const grpcBizMessage = "失败：\"\\\n"

var grpcBizExtra = map[string]string{"中文\"\\": "值\n\t\x00"}

type grpcThriftHandler struct {
	downstream      service.Client
	compression     string
	requestMetadata func(context.Context) map[string]string
}

func (h grpcThriftHandler) Unary(ctx context.Context, value *parent.Message) (*parent.Message, error) {
	if err := h.checkCompression(ctx); err != nil {
		return nil, err
	}
	return value, nil
}
func (h grpcThriftHandler) ExplicitUnary(ctx context.Context, value *parent.Message) (*parent.Message, error) {
	if err := h.checkCompression(ctx); err != nil {
		return nil, err
	}
	return value, nil
}
func (grpcThriftHandler) Zero(context.Context) error { return nil }
func (grpcThriftHandler) Multi(_ context.Context, left int32, right int64) (int64, error) {
	return int64(left) + right, nil
}
func (grpcThriftHandler) Container(_ context.Context, values []int64) ([]int64, error) {
	return values, nil
}
func (h grpcThriftHandler) Throw(ctx context.Context, value *parent.Message) (*parent.Message, error) {
	if h.downstream != nil {
		if string(value.Payload) == "biz" {
			if err := responseEvent(map[string]any{"event": "biz-forwarded"}); err != nil {
				return nil, err
			}
		}
		return h.downstream.Throw(ctx, value)
	}
	switch string(value.Payload) {
	case "biz":
		if err := responseEvent(map[string]any{"event": "biz-emitted"}); err != nil {
			return nil, err
		}
		return nil, kerrors.NewGRPCBizStatusErrorWithExtra(409, grpcBizMessage, grpcBizExtra)
	case "failure":
		return nil, &parent.Failure{Message: "declared failure"}
	case "other":
		return nil, &parent.Other{Code: 7}
	default:
		return value, nil
	}
}
func (grpcThriftHandler) VoidThrow(context.Context) error {
	return &parent.Failure{Message: "void failure"}
}
func (h grpcThriftHandler) Client(ctx context.Context, stream api.Interop_ClientServer) error {
	if err := h.checkCompression(ctx); err != nil {
		return err
	}
	var payload []byte
	var count int64
	for {
		value, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return err
		}
		payload = append(payload, value.Payload...)
		count++
	}
	return stream.SendAndClose(ctx, &parent.Message{Payload: payload, Sequence: &count})
}
func (h grpcThriftHandler) Inherited(ctx context.Context, value *parent.Message, stream parent.Parent_InheritedServer) error {
	if err := h.checkCompression(ctx); err != nil {
		return err
	}
	for i := int64(0); i < 3; i++ {
		if err := stream.Send(ctx, &parent.Message{Payload: value.Payload, Sequence: &i}); err != nil {
			return err
		}
	}
	return nil
}
func (h grpcThriftHandler) Server(ctx context.Context, value *parent.Message, stream api.Interop_ServerServer) error {
	if err := h.checkCompression(ctx); err != nil {
		return err
	}
	for i := int64(0); i < 3; i++ {
		if err := stream.Send(ctx, &parent.Message{Payload: value.Payload, Sequence: &i}); err != nil {
			return err
		}
	}
	return nil
}
func (h grpcThriftHandler) Bidi(ctx context.Context, stream api.Interop_BidiServer) error {
	if err := h.checkCompression(ctx); err != nil {
		return err
	}
	for {
		value, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if strings.HasPrefix(string(value.Payload), "lifecycle|") {
			return h.grpcPending(ctx, stream, value)
		}
		if err := stream.Send(ctx, value); err != nil {
			return err
		}
	}
}

func serveGRPCThrift(profile Profile, address, forward, compression string, legacy bool, additional ...server.Option) error {
	network, target, err := networkAddress(address)
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
		info := service.NewServiceInfo()
		if legacy {
			info = oldservice.NewServiceInfo()
		}
		options = profile.ServerOptions(info)
	}
	options = append(options, server.WithListener(listener))
	options = append(options, additional...)
	handler := &grpcThriftHandler{compression: compression, requestMetadata: profile.RequestMetadata}
	if forward != "" {
		handler.downstream, err = newGRPCClient(profile, forward)
		if err != nil {
			return err
		}
	}
	var svr server.Server
	if legacy {
		svr = oldservice.NewServer(&grpcLegacyHandler{*handler}, options...)
	} else {
		svr = service.NewServer(handler, options...)
	}
	server.RegisterStartHook(func() {
		_ = responseEvent(map[string]any{"event": "ready", "protocol": "grpc-thrift", "addr": network + "://" + listener.Addr().String(), "profile": profile.Name, "kitex_module": profile.KitexModule})
	})
	return svr.Run()
}

func grpcUnknownRoute(_ context.Context, method string, stream streaming.Stream) error {
	request := new(parent.Message)
	if err := stream.RecvMsg(request); err != nil {
		return err
	}
	if err := stream.SetHeader(metadata.Pairs("x-interop-fallback-method", method)); err != nil {
		return err
	}
	return stream.SendMsg(request)
}

func callGRPCThrift(profile Profile, address, report string, timeout time.Duration, compression string, legacy bool) error {
	_, target, err := networkAddress(address)
	if err != nil {
		return err
	}
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(service.NewServiceInfo(), "Interop")
	}
	options = append(options, client.WithTransportProtocol(transport.GRPC), client.WithHostPorts(target))
	var c service.Client
	if legacy {
		c, err = newLegacyGRPCClient(profile, address, compression)
	} else {
		c, err = service.NewClient("Interop", options...)
	}
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	message := func(payload string) *parent.Message {
		sequence := int64(7)
		return &parent.Message{Payload: []byte(payload), Sequence: &sequence}
	}
	var unaryOptions []callopt.Option
	var streamOptions []streamcall.Option
	if compression == "gzip" {
		unaryOptions = append(unaryOptions, callopt.WithGRPCCompressor("gzip"))
		streamOptions = append(streamOptions, streamcall.WithGRPCCompressor("gzip"))
	} else if compression != "identity" {
		return fmt.Errorf("unknown compression %q", compression)
	}
	var passed []string
	actual, err := c.Unary(ctx, message("raw\x00payload"), unaryOptions...)
	if err != nil {
		return fmt.Errorf("unary: %w", err)
	}
	if !reflect.DeepEqual(actual, message("raw\x00payload")) {
		return fmt.Errorf("unary value: %#v", actual)
	}
	passed = append(passed, "unary-args-result")
	actual, err = c.ExplicitUnary(ctx, message("explicit"), unaryOptions...)
	if err != nil || !reflect.DeepEqual(actual, message("explicit")) {
		return fmt.Errorf("explicit unary: %#v, %v", actual, err)
	}
	passed = append(passed, "explicit-unary")
	if err := c.Zero(ctx, unaryOptions...); err != nil {
		return fmt.Errorf("zero/void: %w", err)
	}
	passed = append(passed, "zero-void-stop")
	scalar, err := c.Multi(ctx, -3, math.MaxInt64, unaryOptions...)
	if err != nil || scalar != math.MaxInt64-3 {
		return fmt.Errorf("multi: %d, %v", scalar, err)
	}
	passed = append(passed, "multiple-parameters-field-ids")
	values := []int64{math.MinInt64, 0, math.MaxInt64}
	container, err := c.Container(ctx, values, unaryOptions...)
	if err != nil || !reflect.DeepEqual(container, values) {
		return fmt.Errorf("container: %v, %v", container, err)
	}
	passed = append(passed, "container-result")
	_, err = c.Throw(ctx, message("failure"), unaryOptions...)
	var failure *parent.Failure
	if !errors.As(err, &failure) || failure.Message != "declared failure" {
		return fmt.Errorf("declared failure: %v", err)
	}
	_, err = c.Throw(ctx, message("other"), unaryOptions...)
	var other *parent.Other
	if !errors.As(err, &other) || other.Code != 7 {
		return fmt.Errorf("declared other: %v", err)
	}
	actual, err = c.Throw(ctx, message("success"), unaryOptions...)
	if err != nil || !reflect.DeepEqual(actual, message("success")) {
		return fmt.Errorf("declared success: %#v, %v", actual, err)
	}
	passed = append(passed, "multiple-declared-exceptions")
	err = c.VoidThrow(ctx, unaryOptions...)
	if !errors.As(err, &failure) || failure.Message != "void failure" {
		return fmt.Errorf("declared void failure: %v", err)
	}
	passed = append(passed, "single-declared-void-exception")
	_, err = c.Throw(ctx, message("biz"), unaryOptions...)
	biz, ok := kerrors.FromBizStatusError(err)
	if !ok || biz.BizStatusCode() != 409 || biz.BizMessage() != grpcBizMessage || !reflect.DeepEqual(biz.BizExtra(), grpcBizExtra) {
		return fmt.Errorf("business JSON round trip: %#v, %v", biz, err)
	}
	passed = append(passed, "biz-status-unicode-json")
	for count := 0; count <= 2; count++ {
		stream, err := c.Client(ctx, streamOptions...)
		if err != nil {
			return err
		}
		var expected []byte
		for i := 0; i < count; i++ {
			value := message("a")
			expected = append(expected, value.Payload...)
			if err := stream.Send(ctx, value); err != nil {
				return err
			}
		}
		value, err := stream.CloseAndRecv(ctx)
		if err != nil || string(value.Payload) != string(expected) || value.GetSequence() != int64(count) {
			return fmt.Errorf("client stream: %#v, %v", value, err)
		}
	}
	passed = append(passed, "client-stream-zero-one-many")
	for _, inherited := range []bool{false, true} {
		var receive func(context.Context) (*parent.Message, error)
		if inherited {
			stream, err := c.Inherited(ctx, message("server"), streamOptions...)
			if err != nil {
				return err
			}
			receive = stream.Recv
		} else {
			stream, err := c.Server(ctx, message("server"), streamOptions...)
			if err != nil {
				return err
			}
			receive = stream.Recv
		}
		for i := int64(0); i < 3; i++ {
			value, err := receive(ctx)
			if err != nil || string(value.Payload) != "server" || value.GetSequence() != i {
				return fmt.Errorf("server stream: %#v, %v", value, err)
			}
		}
		if _, err := receive(ctx); !errors.Is(err, io.EOF) {
			return fmt.Errorf("server terminal: %v", err)
		}
	}
	passed = append(passed, "server-stream-and-inherited-path")
	bidi, err := c.Bidi(ctx, streamOptions...)
	if err != nil {
		return err
	}
	if err := bidi.Send(ctx, message("before-half-close")); err != nil {
		return err
	}
	actual, err = bidi.Recv(ctx)
	if err != nil || !reflect.DeepEqual(actual, message("before-half-close")) {
		return fmt.Errorf("bidi before half close: %#v, %v", actual, err)
	}
	if err := bidi.CloseSend(ctx); err != nil {
		return err
	}
	if _, err := bidi.Recv(ctx); !errors.Is(err, io.EOF) {
		return fmt.Errorf("bidi terminal: %v", err)
	}
	passed = append(passed, "bidi-first-response-before-half-close")
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "PASS", "protocol": "grpc-thrift", "cases": passed, "legacy": legacy, "profile": profile.Name, "kitex_module": profile.KitexModule})
}

func (h grpcThriftHandler) checkCompression(ctx context.Context) error {
	if h.requestMetadata != nil {
		if fields := h.requestMetadata(ctx); fields != nil {
			if err := responseEvent(map[string]any{"event": "request-metadata", "fields": fields}); err != nil {
				return err
			}
		}
	}
	expected := ""
	if h.compression == "gzip" {
		expected = "gzip"
	}
	ri := rpcinfo.GetRPCInfo(ctx)
	if ri == nil || remote.GetRecvCompressor(ri) != expected {
		return fmt.Errorf("request compression differs from fixture configuration")
	}
	return nil
}
