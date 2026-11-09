package oracle

// These adapters exercise the actual legacy generated APIs while sharing all
// scenario assertions and handler behavior with the StreamX fixture.
import (
	"context"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/client/callopt"
	"github.com/cloudwego/kitex/client/callopt/streamcall"
	"github.com/cloudwego/kitex/client/streamclient"
	"github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/metadata"
	"github.com/cloudwego/kitex/pkg/streaming"
	"github.com/cloudwego/kitex/transport"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
	oldparent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/parent"
	oldapi "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api"
	oldservice "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api/interop"
)

func legacyError(err error) error {
	switch value := err.(type) {
	case *parent.Failure:
		return (*oldparent.Failure)(value)
	case *parent.Other:
		return (*oldparent.Other)(value)
	}
	return err
}
func streamXError(err error) error {
	switch value := err.(type) {
	case *oldparent.Failure:
		return (*parent.Failure)(value)
	case *oldparent.Other:
		return (*parent.Other)(value)
	}
	return err
}
func legacyHeaders(values map[string]string) metadata.MD {
	result := metadata.MD{}
	for key, value := range values {
		result[key] = []string{value}
	}
	return result
}
func streamXHeaders(values metadata.MD) map[string]string {
	result := map[string]string{}
	for key, value := range values {
		if len(value) > 0 {
			result[key] = value[0]
		}
	}
	return result
}

type legacyServerStream struct{ streaming.Stream }

func (s legacyServerStream) SendMsg(_ context.Context, value any) error {
	return s.Stream.SendMsg(value)
}
func (s legacyServerStream) RecvMsg(_ context.Context, value any) error {
	return s.Stream.RecvMsg(value)
}
func (s legacyServerStream) SetHeader(value streaming.Header) error {
	return s.Stream.SetHeader(legacyHeaders(value))
}
func (s legacyServerStream) SendHeader(value streaming.Header) error {
	return s.Stream.SendHeader(legacyHeaders(value))
}
func (s legacyServerStream) SetTrailer(value streaming.Trailer) error {
	s.Stream.SetTrailer(legacyHeaders(value))
	return nil
}

type grpcLegacyHandler struct{ common grpcThriftHandler }

func (h grpcLegacyHandler) Unary(ctx context.Context, value *oldparent.Message) (*oldparent.Message, error) {
	result, err := h.common.Unary(ctx, (*parent.Message)(value))
	return (*oldparent.Message)(result), legacyError(err)
}
func (h grpcLegacyHandler) ExplicitUnary(ctx context.Context, value *oldparent.Message) (*oldparent.Message, error) {
	result, err := h.common.ExplicitUnary(ctx, (*parent.Message)(value))
	return (*oldparent.Message)(result), legacyError(err)
}
func (h grpcLegacyHandler) Zero(ctx context.Context) error { return h.common.Zero(ctx) }
func (h grpcLegacyHandler) Multi(ctx context.Context, a int32, b int64) (int64, error) {
	return h.common.Multi(ctx, a, b)
}
func (h grpcLegacyHandler) Container(ctx context.Context, value []int64) ([]int64, error) {
	return h.common.Container(ctx, value)
}
func (h grpcLegacyHandler) Throw(ctx context.Context, value *oldparent.Message) (*oldparent.Message, error) {
	result, err := h.common.Throw(ctx, (*parent.Message)(value))
	return (*oldparent.Message)(result), legacyError(err)
}
func (h grpcLegacyHandler) VoidThrow(ctx context.Context) error {
	return legacyError(h.common.VoidThrow(ctx))
}
func (h grpcLegacyHandler) Client(s oldapi.Interop_ClientServer) error {
	return h.common.Client(s.Context(), streaming.NewClientStreamingServer[parent.Message, parent.Message](legacyServerStream{s}))
}
func (h grpcLegacyHandler) Server(value *oldparent.Message, s oldapi.Interop_ServerServer) error {
	return h.common.Server(s.Context(), (*parent.Message)(value), streaming.NewServerStreamingServer[parent.Message](legacyServerStream{s}))
}
func (h grpcLegacyHandler) Inherited(value *oldparent.Message, s oldparent.Parent_InheritedServer) error {
	return h.common.Inherited(s.Context(), (*parent.Message)(value), streaming.NewServerStreamingServer[parent.Message](legacyServerStream{s}))
}
func (h grpcLegacyHandler) Bidi(s oldapi.Interop_BidiServer) error {
	return h.common.Bidi(s.Context(), streaming.NewBidiStreamingServer[parent.Message, parent.Message](legacyServerStream{s}))
}

type legacyClientStream struct{ streaming.Stream }

func (s legacyClientStream) SendMsg(_ context.Context, value any) error {
	return s.Stream.SendMsg(value)
}
func (s legacyClientStream) RecvMsg(_ context.Context, value any) error {
	return s.Stream.RecvMsg(value)
}
func (s legacyClientStream) Header() (streaming.Header, error) {
	values, err := s.Stream.Header()
	return streamXHeaders(values), err
}
func (s legacyClientStream) Trailer() (streaming.Trailer, error) {
	return streamXHeaders(s.Stream.Trailer()), nil
}
func (s legacyClientStream) CloseSend(context.Context) error { return s.Stream.Close() }

type grpcLegacyClient struct {
	unary       oldservice.Client
	streams     oldservice.StreamClient
	compression string
}

func newLegacyGRPCClient(profile Profile, address, compression string) (service.Client, error) {
	_, target, err := networkAddress(address)
	if err != nil {
		return nil, err
	}
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(oldservice.NewServiceInfoForClient(), "Interop")
	}
	options = append(options, client.WithTransportProtocol(transport.GRPC), client.WithHostPorts(target))
	unary, err := oldservice.NewClient("Interop", options...)
	if err != nil {
		return nil, err
	}
	var streamOptions []streamclient.Option
	// Kitex suites may contain single-use OptionOnce state. Each generated
	// client needs a fresh suite, even when both clients share one destination.
	var freshOptions []client.Option
	if profile.ClientOptions != nil {
		freshOptions = profile.ClientOptions(oldservice.NewServiceInfoForStreamClient(), "Interop")
	}
	freshOptions = append(freshOptions, client.WithTransportProtocol(transport.GRPC), client.WithHostPorts(target))
	for _, option := range freshOptions {
		streamOptions = append(streamOptions, streamclient.ConvertOptionFrom(option))
	}
	streams, err := oldservice.NewStreamClient("Interop", streamOptions...)
	if err != nil {
		return nil, err
	}
	return grpcLegacyClient{unary, streams, compression}, nil
}
func (c grpcLegacyClient) Unary(ctx context.Context, value *parent.Message, opts ...callopt.Option) (*parent.Message, error) {
	result, err := c.unary.Unary(ctx, (*oldparent.Message)(value), opts...)
	return (*parent.Message)(result), streamXError(err)
}
func (c grpcLegacyClient) ExplicitUnary(ctx context.Context, value *parent.Message, opts ...callopt.Option) (*parent.Message, error) {
	ctx = client.NewCtxWithCallOptions(ctx, opts)
	var streamOpts []streamcall.Option
	if c.compression == "gzip" {
		streamOpts = append(streamOpts, streamcall.WithGRPCCompressor("gzip"))
	}
	result, err := c.streams.ExplicitUnary(ctx, (*oldparent.Message)(value), streamOpts...)
	return (*parent.Message)(result), streamXError(err)
}
func (c grpcLegacyClient) Zero(ctx context.Context, opts ...callopt.Option) error {
	return c.unary.Zero(ctx, opts...)
}
func (c grpcLegacyClient) Multi(ctx context.Context, a int32, b int64, opts ...callopt.Option) (int64, error) {
	return c.unary.Multi(ctx, a, b, opts...)
}
func (c grpcLegacyClient) Container(ctx context.Context, values []int64, opts ...callopt.Option) ([]int64, error) {
	return c.unary.Container(ctx, values, opts...)
}
func (c grpcLegacyClient) Throw(ctx context.Context, value *parent.Message, opts ...callopt.Option) (*parent.Message, error) {
	result, err := c.unary.Throw(ctx, (*oldparent.Message)(value), opts...)
	return (*parent.Message)(result), streamXError(err)
}
func (c grpcLegacyClient) VoidThrow(ctx context.Context, opts ...callopt.Option) error {
	return streamXError(c.unary.VoidThrow(ctx, opts...))
}
func (c grpcLegacyClient) Client(ctx context.Context, opts ...streamcall.Option) (service.Interop_ClientClient, error) {
	s, err := c.streams.Client(ctx, opts...)
	if err != nil {
		return nil, err
	}
	return streaming.NewClientStreamingClient[parent.Message, parent.Message](legacyClientStream{s}), nil
}
func (c grpcLegacyClient) Server(ctx context.Context, value *parent.Message, opts ...streamcall.Option) (service.Interop_ServerClient, error) {
	s, err := c.streams.Server(ctx, (*oldparent.Message)(value), opts...)
	if err != nil {
		return nil, err
	}
	return streaming.NewServerStreamingClient[parent.Message](legacyClientStream{s}), nil
}
func (c grpcLegacyClient) Inherited(ctx context.Context, value *parent.Message, opts ...streamcall.Option) (service.Parent_InheritedClient, error) {
	s, err := c.streams.Inherited(ctx, (*oldparent.Message)(value), opts...)
	if err != nil {
		return nil, err
	}
	return streaming.NewServerStreamingClient[parent.Message](legacyClientStream{s}), nil
}
func (c grpcLegacyClient) Bidi(ctx context.Context, opts ...streamcall.Option) (service.Interop_BidiClient, error) {
	s, err := c.streams.Bidi(ctx, opts...)
	if err != nil {
		return nil, err
	}
	return streaming.NewBidiStreamingClient[parent.Message, parent.Message](legacyClientStream{s}), nil
}
