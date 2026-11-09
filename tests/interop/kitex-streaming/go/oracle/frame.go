package oracle

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"math"
	"strconv"

	"github.com/cloudwego/gopkg/bufiox"
	"github.com/cloudwego/gopkg/protocol/thrift"
	"github.com/cloudwego/gopkg/protocol/ttheader"
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

type frameSpec struct {
	SID         int32             `json:"sid"`
	Kind        int               `json:"kind"`
	Method      string            `json:"method"`
	Metadata    map[string]string `json:"metadata"`
	MetadataHex map[string]string `json:"metadata_hex,omitempty"`
	PayloadHex  string            `json:"payload_hex"`
}

func frameBytes(spec frameSpec) ([]byte, error) {
	payload, err := hex.DecodeString(spec.PayloadHex)
	if err != nil {
		return nil, err
	}
	metadata := make(map[string]string, len(spec.Metadata)+len(spec.MetadataHex))
	for key, value := range spec.Metadata {
		metadata[key] = value
	}
	for key, encoded := range spec.MetadataHex {
		if _, exists := metadata[key]; exists {
			return nil, fmt.Errorf("metadata key %q has both text and hex values", key)
		}
		value, err := hex.DecodeString(encoded)
		if err != nil {
			return nil, fmt.Errorf("metadata key %q: %w", key, err)
		}
		metadata[key] = string(value)
	}
	var b []byte
	w := bufiox.NewBytesWriter(&b)
	length, err := ttheader.Encode(context.Background(), ttheader.EncodeParam{
		Flags: ttheader.HeaderFlagsStreaming, SeqID: spec.SID, ProtocolID: ttheader.ProtocolIDThriftStruct,
		IntInfo: map[uint16]string{ttheader.FrameType: strconv.Itoa(spec.Kind), ttheader.ToMethod: spec.Method},
		StrInfo: metadata,
	}, w)
	if err != nil {
		return nil, err
	}
	if _, err = w.WriteBinary(payload); err != nil {
		return nil, err
	}
	binary.BigEndian.PutUint32(length, uint32(w.WrittenLen()-4))
	if err = w.Flush(); err != nil {
		return nil, err
	}
	return b, nil
}

func decodeFrame(b []byte) (map[string]any, error) {
	reader := bufiox.NewBytesReader(b)
	param, err := ttheader.Decode(context.Background(), reader)
	if err != nil {
		return nil, err
	}
	payload := make([]byte, param.PayloadLen)
	if _, err = reader.ReadBinary(payload); err != nil {
		return nil, err
	}
	frame, err := ttstream.DecodeFrame(context.Background(), bufiox.NewBytesReader(b))
	if err != nil {
		return nil, err
	}
	decoded := map[string]any{"sid": param.SeqID, "flags": param.Flags, "protocol_id": param.ProtocolID, "int_metadata": param.IntInfo,
		"metadata": param.StrInfo, "payload_hex": hex.EncodeToString(payload), "kitex_frame": frame.String()}
	intHex, stringHex := make(map[string]string), make(map[string]string)
	for key, value := range param.IntInfo {
		intHex[strconv.Itoa(int(key))] = hex.EncodeToString([]byte(value))
	}
	for key, value := range param.StrInfo {
		stringHex[hex.EncodeToString([]byte(key))] = hex.EncodeToString([]byte(value))
	}
	decoded["raw_metadata"] = map[string]any{"int_hex": intHex, "string_hex": stringHex}
	if param.IntInfo[ttheader.FrameType] == ttheader.FrameTypeData {
		var value message.Message
		consumed, err := value.FastRead(payload)
		if err != nil || consumed != len(payload) {
			return nil, fmt.Errorf("decode DATA consumed %d/%d bytes: %v", consumed, len(payload), err)
		}
		decoded["message"] = &value
	}
	if len(payload) > 0 && (param.IntInfo[ttheader.FrameType] == ttheader.FrameTypeTrailer || param.IntInfo[ttheader.FrameType] == ttheader.FrameTypeRst) {
		method, typ, sid, prefix, err := thrift.Binary.ReadMessageBegin(payload)
		if err != nil || typ != thrift.EXCEPTION {
			return nil, fmt.Errorf("terminal payload is not a Thrift EXCEPTION (type %d): %v", typ, err)
		}
		exception := thrift.NewApplicationException(0, "")
		consumed, err := exception.FastRead(payload[prefix:])
		if err != nil || prefix+consumed != len(payload) {
			return nil, fmt.Errorf("decode exception consumed %d/%d bytes: %v", prefix+consumed, len(payload), err)
		}
		decoded["exception"] = map[string]any{"method": method, "sid": sid, "code": exception.TypeId(), "message": exception.Msg()}
	}
	return decoded, nil
}

func frameGolden() (map[string]any, error) {
	var cases []map[string]any
	for _, sid := range []int32{0, 1, -1, math.MinInt32, math.MaxInt32} {
		for kind := 1; kind <= 5; kind++ {
			spec := frameSpec{SID: sid, Kind: kind, Method: "Bidi", Metadata: map[string]string{"custom": "中文"}}
			if kind == 2 {
				spec.Metadata["isn"] = "Interop"
			}
			if kind == 3 {
				spec.Metadata = nil
				spec.PayloadHex = hex.EncodeToString(encoded(&message.Message{Value: "payload", Seq: 42}))
			}
			b, err := frameBytes(spec)
			if err != nil {
				return nil, err
			}
			decoded, err := decodeFrame(b)
			if err != nil {
				return nil, err
			}
			cases = append(cases, map[string]any{"name": fmt.Sprintf("frame-%d-sid-%d", kind, sid), "spec": spec, "hex": hex.EncodeToString(b), "go_decode": decoded})
		}
	}
	return map[string]any{"schemaVersion": 1, "suite": "frames", "kitex_module": "v0.16.4", "cases": cases}, nil
}
