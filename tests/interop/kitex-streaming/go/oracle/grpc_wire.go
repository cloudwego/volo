package oracle

// Raw H2 peers are explicitly distinct from a real Kitex endpoint in reports.
import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"time"

	_ "github.com/cloudwego/kitex/pkg/remote/codec/protobuf/encoding/gzip"
	grpcstatus "github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/status"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	"golang.org/x/net/http2"
	"golang.org/x/net/http2/hpack"
)

type grpcWireInput struct {
	Path            string            `json:"path"`
	Method          string            `json:"method"`
	ContentType     string            `json:"content_type"`
	Headers         map[string]string `json:"headers"`
	HeadersHex      map[string]string `json:"headers_hex"`
	Data            []string          `json:"data"`
	EndStream       bool              `json:"end_stream"`
	HeaderEndStream bool              `json:"header_end_stream"`
	Trailers        map[string]string `json:"trailers"`
}

func readGRPCWireInput(path string) (grpcWireInput, error) {
	var input grpcWireInput
	data, err := os.ReadFile(path)
	if err == nil {
		err = json.Unmarshal(data, &input)
	}
	if err != nil || len(input.HeadersHex) == 0 {
		return input, err
	}
	if input.Headers == nil {
		input.Headers = make(map[string]string, len(input.HeadersHex))
	}
	// JSON strings cannot represent invalid UTF-8 header bytes directly.
	for key, value := range input.HeadersHex {
		decoded, err := hex.DecodeString(value)
		if err != nil {
			return input, err
		}
		input.Headers[key] = string(decoded)
	}
	return input, err
}

func grpcWire(address, inputPath, report string) error {
	input, err := readGRPCWireInput(inputPath)
	if err != nil {
		return err
	}
	network, target, err := networkAddress(address)
	if err != nil {
		return err
	}
	conn, err := net.DialTimeout(network, target, 3*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))
	if _, err = io.WriteString(conn, http2.ClientPreface); err != nil {
		return err
	}
	framer := http2.NewFramer(conn, conn)
	framer.ReadMetaHeaders = hpack.NewDecoder(65536, nil)
	if err = framer.WriteSettings(); err != nil {
		return err
	}
	var encoded bytes.Buffer
	encoder := hpack.NewEncoder(&encoded)
	method := input.Method
	if method == "" {
		method = "POST"
	}
	fields := []hpack.HeaderField{{Name: ":method", Value: method}, {Name: ":scheme", Value: "http"}, {Name: ":authority", Value: "fixture"}, {Name: ":path", Value: input.Path}, {Name: "te", Value: "trailers"}}
	if input.ContentType != "" {
		fields = append(fields, hpack.HeaderField{Name: "content-type", Value: input.ContentType})
	}
	for key, value := range input.Headers {
		fields = append(fields, hpack.HeaderField{Name: key, Value: value})
	}
	for _, field := range fields {
		if err = encoder.WriteField(field); err != nil {
			return err
		}
	}
	if err = framer.WriteHeaders(http2.HeadersFrameParam{StreamID: 1, BlockFragment: encoded.Bytes(), EndHeaders: true, EndStream: input.HeaderEndStream}); err != nil {
		return err
	}
	if len(input.Data) == 0 && input.EndStream && !input.HeaderEndStream {
		if err = framer.WriteData(1, true, nil); err != nil {
			return err
		}
	}
	for index, data := range input.Data {
		payload, err := hex.DecodeString(data)
		if err != nil {
			return err
		}
		if err = framer.WriteData(1, input.EndStream && index == len(input.Data)-1, payload); err != nil {
			return err
		}
	}
	headers, trailers := map[string]string{}, map[string]string{}
	var data bytes.Buffer
	first, ended := true, false
	reset := uint32(0)
	for !ended {
		frame, err := framer.ReadFrame()
		if err != nil {
			return err
		}
		switch frame := frame.(type) {
		case *http2.SettingsFrame:
			if !frame.IsAck() {
				if err = framer.WriteSettingsAck(); err != nil {
					return err
				}
			}
		case *http2.PingFrame:
			if !frame.Flags.Has(http2.FlagPingAck) {
				if err = framer.WritePing(true, frame.Data); err != nil {
					return err
				}
			}
		case *http2.MetaHeadersFrame:
			if frame.StreamID != 1 {
				continue
			}
			out := trailers
			if first {
				out = headers
				first = false
			}
			for _, field := range frame.Fields {
				out[field.Name] = field.Value
			}
			ended = frame.StreamEnded()
		case *http2.DataFrame:
			if frame.StreamID != 1 {
				continue
			}
			if data.Len()+len(frame.Data()) > 64*1024*1024 {
				return fmt.Errorf("raw response exceeds fixture budget")
			}
			data.Write(frame.Data())
			if len(frame.Data()) != 0 {
				_ = framer.WriteWindowUpdate(0, uint32(len(frame.Data())))
				_ = framer.WriteWindowUpdate(1, uint32(len(frame.Data())))
			}
			ended = frame.StreamEnded()
		case *http2.RSTStreamFrame:
			if frame.StreamID == 1 {
				reset = uint32(frame.ErrCode)
				ended = true
			}
		case *http2.GoAwayFrame:
			if frame.ErrCode != http2.ErrCodeNo {
				return fmt.Errorf("peer GOAWAY %v", frame.ErrCode)
			}
		}
	}
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "OBSERVED", "peer": "raw-h2-client", "headers": headers, "trailers": trailers, "data": hex.EncodeToString(data.Bytes()), "reset": reset})
}

func grpcRawResponse(address, inputPath string) error {
	input, err := readGRPCWireInput(inputPath)
	if err != nil {
		return err
	}
	network, target, err := networkAddress(address)
	if err != nil {
		return err
	}
	listener, err := net.Listen(network, target)
	if err != nil {
		return err
	}
	defer listener.Close()
	if err = responseEvent(map[string]any{"event": "ready", "addr": network + "://" + listener.Addr().String(), "peer": "raw-h2-server"}); err != nil {
		return err
	}
	conn, err := listener.Accept()
	if err != nil {
		return err
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(8 * time.Second))
	server := &http2.Server{}
	server.ServeConn(conn, &http2.ServeConnOpts{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, io.LimitReader(r.Body, 32*1024*1024))
		if input.ContentType != "" {
			w.Header().Set("Content-Type", input.ContentType)
		}
		for key, value := range input.Headers {
			w.Header().Set(key, value)
		}
		w.WriteHeader(http.StatusOK)
		if len(input.Trailers) != 0 {
			w.(http.Flusher).Flush()
		}
		for _, fragment := range input.Data {
			data, err := hex.DecodeString(fragment)
			if err != nil {
				panic(err)
			}
			_, _ = w.Write(data)
			w.(http.Flusher).Flush()
		}
		for key, value := range input.Trailers {
			// H2 does not need a Trailer declaration. In particular, emitting a
			// multi-valued "trailer" header would make Kitex StreamX's map
			// conversion fail before the metadata being probed is read.
			w.Header().Set(http.TrailerPrefix+key, value)
		}
	})})
	return nil
}

func grpcResponseProbe(profile Profile, address, mode, report string) error {
	c, err := newGRPCClient(profile, address)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	message := &parent.Message{}
	count := 0
	switch mode {
	case "unary":
		_, err = c.Unary(ctx, message)
		if err == nil {
			count = 1
		}
	case "void":
		err = c.Zero(ctx)
		if err == nil {
			count = 1
		}
	case "client":
		s, open := c.Client(ctx)
		err = open
		if err == nil {
			err = s.Send(ctx, message)
		}
		if err == nil {
			_, err = s.CloseAndRecv(ctx)
			if err == nil {
				count = 1
			}
		}
	case "server", "bidi":
		var recv func(context.Context) (*parent.Message, error)
		if mode == "server" {
			s, open := c.Server(ctx, message)
			err = open
			if err == nil {
				recv = s.Recv
			}
		} else {
			s, open := c.Bidi(ctx)
			err = open
			if err == nil {
				err = s.Send(ctx, message)
			}
			if err == nil {
				err = s.CloseSend(ctx)
			}
			if err == nil {
				recv = s.Recv
			}
		}
		if err == nil {
			for count < 10 {
				_, err = recv(ctx)
				if err != nil {
					break
				}
				count++
			}
		}
		if err == io.EOF {
			err = nil
		}
	default:
		return fmt.Errorf("unknown response probe mode %q", mode)
	}
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "OBSERVED", "code": int(grpcstatus.Code(err)), "messages": count, "actual_error": fmt.Sprint(err), "profile": profile.Name})
}
