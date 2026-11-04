package oracle

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"time"

	"github.com/cloudwego/gopkg/bufiox"
	"github.com/cloudwego/gopkg/protocol/thrift"
	"github.com/cloudwego/gopkg/protocol/ttheader"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

func unaryRequest(seq int32, request *message.Message) ([]byte, error) {
	payload, err := thrift.MarshalFastMsg("Unary", thrift.CALL, seq, &message.InteropUnaryArgs{Request: request})
	if err != nil {
		return nil, err
	}
	var b []byte
	writer := bufiox.NewBytesWriter(&b)
	length, err := ttheader.Encode(context.Background(), ttheader.EncodeParam{
		SeqID: seq, ProtocolID: ttheader.ProtocolIDThriftBinary,
		IntInfo: map[uint16]string{ttheader.ToMethod: "Unary"}, StrInfo: map[string]string{"isn": "Interop"},
	}, writer)
	if err != nil {
		return nil, err
	}
	var framed [4]byte
	binary.BigEndian.PutUint32(framed[:], uint32(len(payload)))
	if _, err = writer.WriteBinary(framed[:]); err != nil {
		return nil, err
	}
	if _, err = writer.WriteBinary(payload); err != nil {
		return nil, err
	}
	binary.BigEndian.PutUint32(length, uint32(writer.WrittenLen()-4))
	if err = writer.Flush(); err != nil {
		return nil, err
	}
	return b, nil
}

func unaryReply(conn net.Conn, seq int32, request *message.Message) ([]byte, error) {
	var prefix [4]byte
	if _, err := io.ReadFull(conn, prefix[:]); err != nil {
		return nil, err
	}
	length := binary.BigEndian.Uint32(prefix[:])
	if length < 10 || length > 16*1024*1024 {
		return nil, fmt.Errorf("invalid unary frame length %d", length)
	}
	b := make([]byte, int(length)+4)
	copy(b, prefix[:])
	if _, err := io.ReadFull(conn, b[4:]); err != nil {
		return b, err
	}
	reader := bufiox.NewBytesReader(b)
	param, err := ttheader.Decode(context.Background(), reader)
	if err != nil {
		return b, err
	}
	if param.Flags&ttheader.HeaderFlagsStreaming != 0 || param.SeqID != seq {
		return b, fmt.Errorf("unary response flags/seq mismatch: %d/%d", param.Flags, param.SeqID)
	}
	payload := make([]byte, param.PayloadLen)
	if _, err = reader.ReadBinary(payload); err != nil {
		return b, err
	}
	if len(payload) < 4 || int(binary.BigEndian.Uint32(payload[:4])) != len(payload)-4 {
		return b, fmt.Errorf("unary response is not framed Thrift")
	}
	payload = payload[4:]
	method, typ, replySeq, prefixLen, err := thrift.Binary.ReadMessageBegin(payload)
	if err != nil || method != "Unary" || typ != thrift.REPLY || replySeq != seq {
		return b, fmt.Errorf("unary response envelope differs: method=%q type=%d seq=%d error=%v", method, typ, replySeq, err)
	}
	var result message.InteropUnaryResult
	consumed, err := result.FastRead(payload[prefixLen:])
	if err != nil || consumed+prefixLen != len(payload) {
		return b, fmt.Errorf("unary result consumed %d/%d bytes: %v", consumed+prefixLen, len(payload), err)
	}
	if result.Success == nil || *result.Success != *request {
		return b, fmt.Errorf("unary response differs: %v", result.Success)
	}
	return b, nil
}

func unaryIdle(profile Profile, addr, report string, idle, timeout time.Duration) error {
	if idle <= 0 || timeout <= 0 {
		return fmt.Errorf("idle and request timeout must be positive")
	}
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	// This function dials exactly once. No pool/fallback can hide an idle close.
	conn, err := net.DialTimeout(network, target, 5*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	result := map[string]any{"event": "result", "suite": "idle-unary", "profile": profile.Name,
		"kitex_module": profile.KitexModule, "addr": addr, "local_addr": conn.LocalAddr().String(),
		"dial_count": 1, "idle_seconds": idle.Seconds()}
	var phases []map[string]any
	for phase := 1; phase <= 2; phase++ {
		if err = conn.SetDeadline(time.Time{}); err != nil {
			break
		}
		start := time.Now()
		time.Sleep(idle)
		row := map[string]any{"phase": phase, "idle_elapsed_seconds": time.Since(start).Seconds()}
		phases = append(phases, row)
		if err = conn.SetDeadline(time.Now().Add(timeout)); err != nil {
			break
		}
		request := &message.Message{Value: "idle-unary-中文", Seq: int32(phase)}
		var encoded []byte
		encoded, err = unaryRequest(int32(phase), request)
		if err != nil {
			break
		}
		row["request_hex"] = hex.EncodeToString(encoded)
		for len(encoded) > 0 {
			var n int
			n, err = conn.Write(encoded)
			if err != nil {
				break
			}
			if n == 0 {
				err = io.ErrShortWrite
				break
			}
			encoded = encoded[n:]
		}
		if err != nil {
			break
		}
		var reply []byte
		reply, err = unaryReply(conn, int32(phase), request)
		row["response_hex"] = hex.EncodeToString(reply)
		if err != nil {
			break
		}
		row["response"] = request
	}
	result["phases"], result["ok"] = phases, err == nil
	if err != nil {
		result["error"] = err.Error()
	}
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	return err
}
