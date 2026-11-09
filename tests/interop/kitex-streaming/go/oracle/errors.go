package oracle

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/kerrors"
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	"github.com/cloudwego/kitex/pkg/serviceinfo"
	"github.com/cloudwego/kitex/transport"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

// Exercise the actual Kitex StreamX client and its outer error mapping. The
// client-only service description advertises a method absent from both servers.
func unknownMethod(profile Profile, addr, report string) error {
	_, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	info := *service.NewServiceInfo()
	methods := make(map[string]serviceinfo.MethodInfo, len(info.Methods)+1)
	for name, method := range info.Methods {
		methods[name] = method
	}
	methods["AbsentMethod"] = methods["Bidi"]
	info.Methods = methods
	var options []client.Option
	if profile.ClientOptions != nil {
		options = profile.ClientOptions(&info, "Interop")
	}
	options = append(options, client.WithDestService("Interop"), client.WithHostPorts(target),
		client.WithTransportProtocol(transport.TTHeaderStreaming),
		client.WithTTHeaderStreamingOptions(client.WithTTHeaderStreamingTransportOptions(
			ttstream.WithClientMuxConnPool(ttstream.MuxConnConfig{PoolSize: 1, MaxIdleTimeout: time.Minute}))))
	core, err := client.NewClient(&info, options...)
	if err != nil {
		return err
	}
	if closer, ok := core.(interface{ Close() error }); ok {
		defer closer.Close()
	}
	streamingClient := core.(client.Streaming)
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	start := time.Now()
	stream, err := streamingClient.StreamX(ctx, "AbsentMethod")
	result := map[string]any{"event": "result", "suite": "unknown-method", "profile": profile.Name,
		"kitex_module": profile.KitexModule, "open_elapsed_seconds": time.Since(start).Seconds()}
	if err == nil {
		var response message.Message
		err = stream.RecvMsg(ctx, &response)
	}
	result["error_elapsed_seconds"] = time.Since(start).Seconds()
	result["deadline_expired"] = ctx.Err() != nil
	if err == nil {
		return fmt.Errorf("unknown method unexpectedly returned a response")
	}
	result["error"] = err.Error()
	result["streaming_timeout"] = errors.Is(err, kerrors.ErrStreamingTimeout)
	var typed interface{ TypeId() int32 }
	if errors.As(err, &typed) {
		result["type_id"] = typed.TypeId()
	}
	// A following valid call uses the same configured Kitex client/pool. The
	// separate wire suite proves same-socket survival without pool redial.
	recoveryContext, recoveryCancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer recoveryCancel()
	recovery, err := streamingClient.StreamX(recoveryContext, "Bidi")
	request := &message.Message{Value: "after-unknown-method", Seq: 1}
	if err == nil {
		err = recovery.SendMsg(recoveryContext, request)
	}
	if err == nil {
		err = recovery.CloseSend(recoveryContext)
	}
	if err == nil {
		var response message.Message
		err = recovery.RecvMsg(recoveryContext, &response)
		if err == nil && response != *request {
			err = fmt.Errorf("recovery response differs: %v", response)
		}
	}
	if err == nil {
		var extra message.Message
		if end := recovery.RecvMsg(recoveryContext, &extra); !errors.Is(end, io.EOF) {
			err = fmt.Errorf("recovery did not end successfully: %v", end)
		}
	}
	result["ok"] = err == nil
	if err != nil {
		result["recovery_error"] = err.Error()
	}
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	return err
}
