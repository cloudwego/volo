package oracle

import (
	"context"
	"fmt"
	"io"
	"net"
	"reflect"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/client/streamclient"
	"github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/metadata"
	"github.com/cloudwego/kitex/server"
	"github.com/cloudwego/kitex/transport"
	api "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api/purestreaming"
)

func metadataValues() metadata.MD {
	return metadata.Pairs("X-Interop-Ascii", "original-value",
		"x-interop-multi", "first", "x-interop-multi", "second",
		"x-interop-opaque-bin", "\x00\xff\x80,\x01", "x-interop-opaque-bin", "second\x00value")
}

func checkMetadataValues(actual metadata.MD, phase string) error {
	for key, expected := range metadataValues() {
		if !reflect.DeepEqual(actual[key], expected) {
			return fmt.Errorf("metadata %s differs: %q", key, actual[key])
		}
	}
	if phase != "" && !reflect.DeepEqual(actual["x-interop-phase"], []string{phase}) {
		return fmt.Errorf("metadata phase differs: %q", actual["x-interop-phase"])
	}
	return nil
}

type grpcMetadataValuesHandler struct{}

func serveGRPCMetadataValues(profile Profile, address string) error {
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
		options = profile.ServerOptions(service.NewServiceInfo())
	}
	options = append(options, server.WithListener(listener))
	svr := service.NewServer(&grpcMetadataValuesHandler{}, options...)
	server.RegisterStartHook(func() {
		_ = responseEvent(map[string]any{"event": "ready", "protocol": "grpc-thrift", "addr": network + "://" + listener.Addr().String(), "profile": profile.Name, "kitex_module": profile.KitexModule})
	})
	return svr.Run()
}

func (*grpcMetadataValuesHandler) Bidi(stream api.PureStreaming_BidiServer) error {
	fields, _ := metadata.FromIncomingContext(stream.Context())
	if _, probe := fields["x-interop-ascii"]; probe {
		if err := checkMetadataValues(fields, ""); err != nil {
			return err
		}
		headers, trailers := metadataValues(), metadataValues()
		headers["x-interop-phase"], trailers["x-interop-phase"] = []string{"header"}, []string{"trailer"}
		if err := stream.SetHeader(headers); err != nil {
			return err
		}
		stream.SetTrailer(trailers)
	}
	for {
		value, err := stream.Recv()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		if err := stream.Send(value); err != nil {
			return err
		}
	}
}

func grpcMetadataValuesClient(profile Profile, address, report string) error {
	_, target, err := networkAddress(address)
	if err != nil {
		return err
	}
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(service.NewServiceInfoForStreamClient(), "PureStreaming")
	}
	options = append(options, client.WithTransportProtocol(transport.GRPC), client.WithHostPorts(target))
	var streamOptions []streamclient.Option
	for _, option := range options {
		streamOptions = append(streamOptions, streamclient.ConvertOptionFrom(option))
	}
	c, err := service.NewStreamClient("PureStreaming", streamOptions...)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	ctx = metadata.NewOutgoingContext(ctx, metadataValues())
	stream, err := c.Bidi(ctx)
	if err != nil {
		return err
	}
	if err := stream.Send(&api.Empty{}); err != nil {
		return err
	}
	if err := stream.Close(); err != nil {
		return err
	}
	headers, err := stream.Header()
	if err != nil {
		return err
	}
	if err := checkMetadataValues(headers, "header"); err != nil {
		return err
	}
	if _, err := stream.Recv(); err != nil {
		return err
	}
	if _, err := stream.Recv(); err != io.EOF {
		return fmt.Errorf("metadata probe terminal: %v", err)
	}
	if err := checkMetadataValues(stream.Trailer(), "trailer"); err != nil {
		return err
	}
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "PASS",
		"scope": "real generated legacy gRPC stream; ASCII/repeated/opaque binary Header and Trailer", "cases": []string{"ascii-multi-binary-header-trailer"}})
}
