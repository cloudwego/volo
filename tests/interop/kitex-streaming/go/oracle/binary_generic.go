package oracle

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"time"

	"github.com/cloudwego/kitex/client/genericclient"
	"github.com/cloudwego/kitex/pkg/generic"
	"github.com/cloudwego/kitex/server"
	"github.com/cloudwego/kitex/server/genericserver"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

// binaryGenericMessage accepts exactly the raw Message struct body used by
// BinaryThriftGenericV2 streaming. No Thrift message envelope or Args/Result is added.
func binaryGenericMessage(value interface{}) (*message.Message, error) {
	data, ok := value.([]byte)
	if !ok {
		return nil, fmt.Errorf("binary generic expected []byte, received %T", value)
	}
	m := message.NewMessage()
	n, err := m.FastRead(data)
	if err != nil {
		return nil, fmt.Errorf("binary generic Message decode: %w", err)
	}
	if n != len(data) {
		return nil, fmt.Errorf("binary generic Message consumed %d of %d bytes", n, len(data))
	}
	return m, nil
}

func newBinaryGenericServer(profile Profile, listener net.Listener) (server.Server, error) {
	g := generic.BinaryThriftGenericV2("Interop")
	info := generic.ServiceInfoWithGeneric(g)
	var options []server.Option
	if profile.ServerOptions != nil {
		options = profile.ServerOptions(info)
	}
	options = append(options, server.WithListener(listener))
	svr := server.NewServer(options...)
	// BinaryThriftGenericV2 routes all streaming modes through BidiStreaming.
	if err := genericserver.RegisterService(svr, &generic.ServiceV2{BidiStreaming: binaryGenericBidi}, g); err != nil {
		return nil, err
	}
	return svr, nil
}

func binaryGenericBidi(ctx context.Context, _, method string, stream generic.BidiStreamingServer) error {
	switch method {
	case "Client":
		last, count := "", int32(0)
		for {
			value, err := stream.Recv(ctx)
			if errors.Is(err, io.EOF) {
				return stream.Send(ctx, encoded(&message.Message{Value: last, Seq: count}))
			}
			if err != nil {
				return err
			}
			m, err := binaryGenericMessage(value)
			if err != nil {
				return err
			}
			last = m.Value
			count++
		}
	case "Server":
		value, err := stream.Recv(ctx)
		if err != nil {
			return err
		}
		m, err := binaryGenericMessage(value)
		if err != nil {
			return err
		}
		for i := int32(0); i < m.Seq; i++ {
			if err := stream.Send(ctx, encoded(&message.Message{Value: m.Value, Seq: i})); err != nil {
				return err
			}
		}
		return nil
	case "Bidi":
		for {
			value, err := stream.Recv(ctx)
			if errors.Is(err, io.EOF) {
				return nil
			}
			if err != nil {
				return err
			}
			// Validate the complete body while echoing the original raw bytes.
			if _, err := binaryGenericMessage(value); err != nil {
				return err
			}
			if err := stream.Send(ctx, value); err != nil {
				return err
			}
		}
	default:
		return fmt.Errorf("binary generic fixture unsupported streaming method %q", method)
	}
}

func binaryGenericCall(profile Profile, addr, mode, payload, pool, report string, count int, timeout time.Duration) error {
	g := generic.BinaryThriftGenericV2("Interop")
	options, err := newClientOptions(profile, addr, pool, generic.ServiceInfoWithGeneric(g))
	if err != nil {
		return err
	}
	c, err := genericclient.NewClient("Interop", g, options...)
	if err != nil {
		return err
	}
	defer c.Close()
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	responses := make([]*message.Message, 0)
	start := time.Now()
	appendResponse := func(value interface{}) error {
		m, err := binaryGenericMessage(value)
		if err == nil {
			responses = append(responses, m)
		}
		return err
	}
	switch mode {
	case "client":
		var stream genericclient.ClientStreamingClient
		stream, err = c.ClientStreaming(ctx, "Client")
		if err != nil {
			break
		}
		for i := 0; i < count; i++ {
			err = stream.Send(ctx, encoded(&message.Message{Value: payload, Seq: int32(i)}))
			if err != nil {
				break
			}
		}
		if err == nil {
			var value interface{}
			value, err = stream.CloseAndRecv(ctx)
			if err == nil {
				err = appendResponse(value)
			}
		}
	case "server":
		var stream genericclient.ServerStreamingClient
		stream, err = c.ServerStreaming(ctx, "Server", encoded(&message.Message{Value: payload, Seq: int32(count)}))
		if err != nil {
			break
		}
		for {
			value, recvErr := stream.Recv(ctx)
			if errors.Is(recvErr, io.EOF) {
				break
			}
			if recvErr != nil {
				err = recvErr
				break
			}
			if err = appendResponse(value); err != nil {
				break
			}
		}
	case "bidi":
		var stream genericclient.BidiStreamingClient
		stream, err = c.BidirectionalStreaming(ctx, "Bidi")
		if err != nil {
			break
		}
		sent := make(chan error, 1)
		go func() {
			for i := 0; i < count; i++ {
				if sendErr := stream.Send(ctx, encoded(&message.Message{Value: payload, Seq: int32(i)})); sendErr != nil {
					sent <- sendErr
					return
				}
			}
			sent <- stream.CloseSend(ctx)
		}()
		for {
			value, recvErr := stream.Recv(ctx)
			if errors.Is(recvErr, io.EOF) {
				break
			}
			if recvErr != nil {
				err = recvErr
				break
			}
			if err = appendResponse(value); err != nil {
				break
			}
		}
		if err != nil {
			cancel()
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
		err = fmt.Errorf("binary generic supports client, server and bidi, received %q", mode)
	}
	result := map[string]any{"event": "result", "schemaVersion": 1, "api": "binary", "ok": err == nil, "mode": mode, "pool": pool, "responses": responses, "elapsed_ns": time.Since(start).Nanoseconds(), "status": "ok", "profile": profile.Name, "kitex_module": profile.KitexModule}
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
