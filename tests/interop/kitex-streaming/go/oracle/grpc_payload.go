package oracle

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"reflect"
	"runtime/debug"
	"sort"

	"github.com/cloudwego/gopkg/protocol/thrift"
	"github.com/cloudwego/kitex/pkg/remote"
	grpccodec "github.com/cloudwego/kitex/pkg/remote/codec/grpc"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
	"github.com/cloudwego/kitex/pkg/serviceinfo"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	parentservice "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent/parent"
	api "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api"
	interop "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
	global "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc_no_namespace/global"
	legacy "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen_grpc_legacy/grpc/payload/api/interop"
)

// Only the two FrameWrite methods are used by the real gRPC encoder. Embedding
// ByteBuffer keeps the fixture on the public codec interface without a second
// implementation of the payload encoder.
type grpcFrameBuffer struct {
	remote.ByteBuffer
	header, payload []byte
}

func (b *grpcFrameBuffer) WriteHeader(value []byte) error {
	b.header = append([]byte(nil), value...)
	return nil
}
func (b *grpcFrameBuffer) WriteData(value []byte) error {
	b.payload = append([]byte(nil), value...)
	return nil
}

func grpcPayloadContext() (context.Context, rpcinfo.RPCInfo) {
	cfg := rpcinfo.NewRPCConfig()
	rpcinfo.AsMutableRPCConfig(cfg).SetPayloadCodec(serviceinfo.Thrift)
	invocation := rpcinfo.NewInvocation("Interop", "Bidi", "api")
	invocation.SetStreamingMode(serviceinfo.StreamingBidirectional)
	ri := rpcinfo.NewRPCInfo(rpcinfo.EmptyEndpointInfo(), rpcinfo.EmptyEndpointInfo(), invocation, cfg, rpcinfo.NewRPCStats())
	return rpcinfo.NewCtxWithRPCInfo(context.Background(), ri), ri
}

func grpcEncodePayload(value thrift.FastCodec) ([]byte, error) {
	ctx, ri := grpcPayloadContext()
	message := remote.NewMessage(value, ri, remote.Stream, remote.Client)
	defer message.Recycle()
	output := new(grpcFrameBuffer)
	if err := grpccodec.NewGRPCCodec().Encode(ctx, message, output); err != nil {
		return nil, err
	}
	return append(output.header, output.payload...), nil
}

func grpcDecodePayload(value thrift.FastCodec, frame []byte) error {
	ctx, ri := grpcPayloadContext()
	message := remote.NewMessage(value, ri, remote.Stream, remote.Server)
	defer message.Recycle()
	return grpccodec.NewGRPCCodec().Decode(ctx, message, remote.NewReaderBuffer(frame))
}

func grpcPayloadConstructor(name string) (thrift.FastCodec, error) {
	switch name {
	case "Message":
		return parent.NewMessage(), nil
	case "Empty":
		return api.NewEmpty(), nil
	case "InteropUnaryArgs":
		return api.NewInteropUnaryArgs(), nil
	case "InteropUnaryResult":
		return api.NewInteropUnaryResult(), nil
	case "InteropExplicitUnaryArgs":
		return api.NewInteropExplicitUnaryArgs(), nil
	case "InteropExplicitUnaryResult":
		return api.NewInteropExplicitUnaryResult(), nil
	case "InteropZeroArgs":
		return api.NewInteropZeroArgs(), nil
	case "InteropZeroResult":
		return api.NewInteropZeroResult(), nil
	case "InteropMultiArgs":
		return api.NewInteropMultiArgs(), nil
	case "InteropMultiResult":
		return api.NewInteropMultiResult(), nil
	case "InteropContainerArgs":
		return api.NewInteropContainerArgs(), nil
	case "InteropContainerResult":
		return api.NewInteropContainerResult(), nil
	case "InteropThrowArgs":
		return api.NewInteropThrowArgs(), nil
	case "InteropThrowResult":
		return api.NewInteropThrowResult(), nil
	case "InteropVoidThrowArgs":
		return api.NewInteropVoidThrowArgs(), nil
	case "InteropVoidThrowResult":
		return api.NewInteropVoidThrowResult(), nil
	default:
		return nil, fmt.Errorf("unknown gRPC golden type %q", name)
	}
}

type grpcPayloadCase struct {
	Name     string  `json:"name"`
	Type     string  `json:"type"`
	Hex      string  `json:"hex"`
	FrameHex string  `json:"frame_hex"`
	RustHex  *string `json:"rust_hex,omitempty"`
}

type grpcPayloadFile struct {
	SchemaVersion int               `json:"schemaVersion"`
	Producer      string            `json:"producer"`
	Codec         string            `json:"codec"`
	Modules       map[string]string `json:"modules"`
	Services      []map[string]any  `json:"services"`
	APIs          []map[string]any  `json:"apis"`
	Cases         []grpcPayloadCase `json:"cases"`
	Boundary      []map[string]any  `json:"boundary"`
}

func grpcPayloadGolden() (grpcPayloadFile, error) {
	file := grpcPayloadFile{SchemaVersion: 1, Producer: "kitex-grpc-codec", Codec: "github.com/cloudwego/kitex/pkg/remote/codec/grpc.NewGRPCCodec", Modules: map[string]string{}}
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, dep := range info.Deps {
			if dep.Path == "github.com/cloudwego/kitex" || dep.Path == "github.com/cloudwego/thriftgo" {
				file.Modules[dep.Path] = dep.Version
			}
		}
	}
	for _, info := range []*serviceinfo.ServiceInfo{interop.NewServiceInfo(), parentservice.NewServiceInfo(), global.NewServiceInfo()} {
		var methods []string
		for name := range info.Methods {
			methods = append(methods, name)
		}
		sort.Strings(methods)
		for _, name := range methods {
			file.Services = append(file.Services, map[string]any{
				"service": info.ServiceName, "package": info.Extra["PackageName"],
				"method": name, "path": "/" + info.Extra["PackageName"].(string) + "." + info.ServiceName + "/" + name,
				"mode": info.Methods[name].StreamingMode(), "generator_version": info.KiteXGenVersion,
			})
		}
	}
	for _, api := range []struct {
		name string
		typ  reflect.Type
	}{
		{"streamx.Client", reflect.TypeOf((*interop.Client)(nil)).Elem()},
		{"legacy.Client", reflect.TypeOf((*legacy.Client)(nil)).Elem()},
		{"legacy.StreamClient", reflect.TypeOf((*legacy.StreamClient)(nil)).Elem()},
	} {
		var methods []string
		for i := 0; i < api.typ.NumMethod(); i++ {
			methods = append(methods, api.typ.Method(i).Name)
		}
		file.APIs = append(file.APIs, map[string]any{"name": api.name, "methods": methods})
	}
	seq := int64(-9223372036854775808)
	value := &parent.Message{Payload: []byte("hello\x00中文"), Sequence: &seq}
	ret := int64(9223372036854775807)
	container := []int64{-1, 0, ret}
	for _, item := range []struct {
		name, typ string
		value     thrift.FastCodec
	}{
		{"streaming-raw", "Message", value},
		{"empty-stop", "Empty", api.NewEmpty()},
		{"unary-args", "InteropUnaryArgs", &api.InteropUnaryArgs{Request: value}},
		{"explicit-unary-args", "InteropExplicitUnaryArgs", &api.InteropExplicitUnaryArgs{Request: value}},
		{"unary-result", "InteropUnaryResult", &api.InteropUnaryResult{Success: value}},
		{"explicit-unary-result", "InteropExplicitUnaryResult", &api.InteropExplicitUnaryResult{Success: value}},
		{"zero-args", "InteropZeroArgs", api.NewInteropZeroArgs()},
		{"void-result", "InteropZeroResult", api.NewInteropZeroResult()},
		{"multi-args-non-default-ids", "InteropMultiArgs", &api.InteropMultiArgs{Left: -2147483648, Right: ret}},
		{"scalar-result", "InteropMultiResult", &api.InteropMultiResult{Success: &ret}},
		{"container-args", "InteropContainerArgs", &api.InteropContainerArgs{Values: container}},
		{"container-result", "InteropContainerResult", &api.InteropContainerResult{Success: container}},
		{"throws-args", "InteropThrowArgs", &api.InteropThrowArgs{Request: value}},
		{"throws-success", "InteropThrowResult", &api.InteropThrowResult{Success: value}},
		{"throws-failure-id-2", "InteropThrowResult", &api.InteropThrowResult{Failure: &parent.Failure{Message: "错误\"\\\n"}}},
		{"throws-other-id-7", "InteropThrowResult", &api.InteropThrowResult{Other: &parent.Other{Code: 42}}},
		{"void-throws-args", "InteropVoidThrowArgs", api.NewInteropVoidThrowArgs()},
		{"void-throws-success", "InteropVoidThrowResult", api.NewInteropVoidThrowResult()},
		{"void-throws-failure-id-4", "InteropVoidThrowResult", &api.InteropVoidThrowResult{Failure: &parent.Failure{Message: "failure"}}},
	} {
		frame, err := grpcEncodePayload(item.value)
		if err != nil {
			return file, fmt.Errorf("%s encode: %w", item.name, err)
		}
		decoded, _ := grpcPayloadConstructor(item.typ)
		if err := grpcDecodePayload(decoded, frame); err != nil {
			return file, fmt.Errorf("%s decode: %w", item.name, err)
		}
		file.Cases = append(file.Cases, grpcPayloadCase{Name: item.name, Type: item.typ, Hex: hex.EncodeToString(frame[5:]), FrameHex: hex.EncodeToString(frame)})
	}
	// Raw malformed payloads are explicitly separate from generated messages.
	for _, item := range []struct {
		name, typ string
		payload   []byte
	}{
		{"zero-bytes", "Empty", nil},
		{"empty-result-without-success", "InteropUnaryResult", []byte{0}},
		{"multiple-result-fields", "InteropThrowResult", encoded(&api.InteropThrowResult{Success: value, Other: &parent.Other{Code: 42}})},
		{"valid-stop-with-suffix", "Empty", []byte{0, 0xa1, 0xb2}},
	} {
		frame := make([]byte, 5, 5+len(item.payload))
		binary.BigEndian.PutUint32(frame[1:], uint32(len(item.payload)))
		frame = append(frame, item.payload...)
		decoded, _ := grpcPayloadConstructor(item.typ)
		err := grpcDecodePayload(decoded, frame)
		row := map[string]any{"name": item.name, "type": item.typ, "hex": hex.EncodeToString(item.payload), "accepted": err == nil}
		if err != nil {
			row["error"] = err.Error()
		}
		file.Boundary = append(file.Boundary, row)
	}
	return file, nil
}

func grpcVerifyPayload(path string) error {
	bytes, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var file grpcPayloadFile
	if err := json.Unmarshal(bytes, &file); err != nil {
		return err
	}
	if file.Producer != "rust" || file.SchemaVersion != 1 || len(file.Cases) == 0 {
		return fmt.Errorf("expected a nonempty Rust-produced schema 1 report")
	}
	expected, err := grpcPayloadGolden()
	if err != nil {
		return err
	}
	remaining := make(map[string]grpcPayloadCase, len(expected.Cases))
	for _, row := range expected.Cases {
		remaining[row.Name] = row
	}
	for _, row := range file.Cases {
		original, ok := remaining[row.Name]
		if !ok || row.Type != original.Type || row.Hex != original.Hex {
			return fmt.Errorf("%s: duplicate, unknown, or altered golden case", row.Name)
		}
		delete(remaining, row.Name)
		if row.RustHex == nil {
			return fmt.Errorf("%s: no Rust-produced bytes", row.Name)
		}
		payload, err := hex.DecodeString(*row.RustHex)
		if err != nil {
			return err
		}
		frame := make([]byte, 5, 5+len(payload))
		binary.BigEndian.PutUint32(frame[1:], uint32(len(payload)))
		frame = append(frame, payload...)
		value, err := grpcPayloadConstructor(row.Type)
		if err != nil {
			return err
		}
		if err := grpcDecodePayload(value, frame); err != nil {
			return fmt.Errorf("%s: %w", row.Name, err)
		}
		encoded, err := grpcEncodePayload(value)
		if err != nil {
			return err
		}
		if hex.EncodeToString(encoded[5:]) != row.Hex {
			return fmt.Errorf("%s: Go re-encoding differs from golden", row.Name)
		}
	}
	if len(remaining) != 0 {
		return fmt.Errorf("missing %d Rust payload cases", len(remaining))
	}
	return nil
}
