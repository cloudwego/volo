package oracle

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"time"

	"github.com/cloudwego/gopkg/protocol/thrift"
	"github.com/cloudwego/gopkg/protocol/ttheader"
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

// responseProbe records the actual API result, including expected failures.
// Assertions belong to the shared runner, never to this observation endpoint.
func responseProbe(profile Profile, addr, pool, report string) error {
	c, err := newTypedClient(profile, addr, pool)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	start := time.Now()
	stream, err := c.Bidi(ctx)
	openElapsed := time.Since(start).Seconds()
	stage := "open"
	var responses []*message.Message
	var returns []map[string]any
	if err == nil {
		stage = "send"
		err = stream.Send(ctx, &message.Message{Value: "response-probe", Seq: 1})
	}
	if err == nil {
		stage = "close-send"
		err = stream.CloseSend(ctx)
	}
	if err == nil {
		stage = "recv"
		for i := 0; i < 8; i++ {
			var value *message.Message
			value, err = stream.Recv(ctx)
			row := map[string]any{"message": value, "error": nil}
			if err != nil {
				row["error"] = err.Error()
			}
			returns = append(returns, row)
			// Go may return a decoded DATA together with a BizStatusErr. Keep
			// its actual return in recv_returns; don't invent a successful Recv.
			if value != nil && (err == nil || value.Value != "" || value.Seq != 0) {
				responses = append(responses, value)
			}
			if err != nil {
				break
			}
		}
	}
	result := map[string]any{"schemaVersion": 1, "event": "result", "api": "response-probe",
		"profile": profile.Name, "kitex_module": profile.KitexModule, "pool": pool,
		"responses": responses, "recv_returns": returns, "terminal": observedErrorStatus(err),
		"stage": stage, "open_elapsed_seconds": openElapsed, "ok": err != nil}
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	if err == nil {
		return fmt.Errorf("no terminal within 8 receive calls")
	}
	return nil
}

func responseEvent(value any) error {
	b, err := json.Marshal(value)
	if err == nil {
		_, err = fmt.Println(string(b))
	}
	return err
}

// rawResponseServer is deliberately a Go-codec peer, not a Kitex server. It
// waits for the complete client request before responding: a client that waits
// for a response HEADER inside open cannot complete this fixture.
func rawResponseServer(profile Profile, addr, input string) error {
	b, err := os.ReadFile(input)
	if err != nil {
		return err
	}
	var scenario WireScenario
	if err = json.Unmarshal(b, &scenario); err != nil {
		return err
	}
	if scenario.SchemaVersion != 1 || len(scenario.Actions) == 0 || len(scenario.Actions) > 32 {
		return fmt.Errorf("invalid response fixture schema/actions")
	}
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	listener, err := net.Listen(network, target)
	if err != nil {
		return err
	}
	defer listener.Close()
	if err = responseEvent(map[string]any{"event": "ready", "addr": network + "://" + listener.Addr().String(),
		"profile": profile.Name, "kitex_module": profile.KitexModule, "api": "go-codec-response-peer"}); err != nil {
		return err
	}
	conn, err := listener.Accept()
	if err != nil {
		return err
	}
	defer conn.Close()
	if err = conn.SetDeadline(time.Now().Add(10 * time.Second)); err != nil {
		return err
	}
	var sid int32
	var method string
	var incoming []map[string]any
	requestComplete := false
	for i := 0; i < 8; i++ {
		var prefix [4]byte
		if _, err = io.ReadFull(conn, prefix[:]); err != nil {
			return err
		}
		length := binary.BigEndian.Uint32(prefix[:])
		if length < 10 || length > 1024*1024 {
			return fmt.Errorf("invalid request frame length %d", length)
		}
		frame := make([]byte, int(length)+4)
		copy(frame, prefix[:])
		if _, err = io.ReadFull(conn, frame[4:]); err != nil {
			return err
		}
		decoded, decodeErr := decodeFrame(frame)
		if decodeErr != nil {
			return decodeErr
		}
		decoded["hex"] = hex.EncodeToString(frame)
		incoming = append(incoming, decoded)
		meta := decoded["int_metadata"].(map[uint16]string)
		if i == 0 {
			if meta[27] != "2" {
				return fmt.Errorf("first client frame was not HEADER")
			}
			sid, method = decoded["sid"].(int32), meta[ttheader.ToMethod]
		}
		if decoded["sid"].(int32) != sid {
			return fmt.Errorf("unexpected concurrent stream in single-stream response fixture")
		}
		if meta[27] == "4" {
			requestComplete = true
			break
		}
	}
	if !requestComplete {
		return fmt.Errorf("request did not close send within 8 frames")
	}
	var sent []map[string]any
	var bytes []byte
	for _, action := range scenario.Actions {
		if action.Op != "send" || (action.Message != nil && action.Exception != nil) {
			return fmt.Errorf("invalid response action")
		}
		action.Frame.SID, action.Frame.Method = sid, method
		if action.Message != nil {
			action.Frame.PayloadHex = hex.EncodeToString(encoded(action.Message))
		}
		if action.Exception != nil {
			payload, encodeErr := ttstream.EncodeException(context.Background(), method, sid,
				thrift.NewApplicationException(action.Exception.Code, action.Exception.Message))
			if encodeErr != nil {
				return encodeErr
			}
			action.Frame.PayloadHex = hex.EncodeToString(payload)
		}
		action.Frame.PayloadHex += action.PayloadSuffix
		frame, encodeErr := frameBytes(action.Frame)
		if encodeErr != nil {
			return encodeErr
		}
		sent = append(sent, map[string]any{"kind": action.Frame.Kind, "sid": sid, "hex": hex.EncodeToString(frame)})
		bytes = append(bytes, frame...)
	}
	for len(bytes) > 0 {
		n, writeErr := conn.Write(bytes)
		if writeErr != nil {
			return writeErr
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		bytes = bytes[n:]
	}
	if err = responseEvent(map[string]any{"event": "wire_sent", "received": incoming, "sent": sent,
		"request_complete_before_response": true}); err != nil {
		return err
	}
	// Keep the peer alive until the client exits; immediate close can obscure
	// the actual terminal with Go's OnInactive cancellation (12013).
	_, _ = io.Copy(io.Discard, conn)
	return nil
}
