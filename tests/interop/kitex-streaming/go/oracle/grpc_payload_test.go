package oracle

import (
	"encoding/binary"
	"encoding/hex"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/cloudwego/gopkg/protocol/thrift"
	codec "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/codec"
)

func TestGRPCMalformedPayloadBoundaries(t *testing.T) {
	for _, item := range []struct {
		name     string
		newValue func() thrift.FastCodec
		payload  []byte
		accepted bool
	}{
		{"required-missing", func() thrift.FastCodec { return codec.NewNested() }, []byte{0}, false},
		{"required-zero-present", func() thrift.FastCodec { return codec.NewNested() }, []byte{8, 0, 1, 0, 0, 0, 0, 0}, true},
		{"negative-binary", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{11, 0, 5, 255, 255, 255, 255, 0}, false},
		{"truncated-binary", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{11, 0, 5, 0, 0, 0, 8, 'x', 0}, false},
		{"negative-list", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{15, 0, 99, 10, 255, 255, 255, 255, 0}, false},
		{"negative-set", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{14, 0, 99, 10, 255, 255, 255, 255, 0}, false},
		{"negative-map", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{13, 0, 99, 11, 10, 255, 255, 255, 255, 0}, false},
		{"large-claimed-list", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte{15, 0, 99, 10, 127, 255, 255, 255, 0}, false},
		{"within-depth", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte(strings.Repeat("\x0c\x00\x63", 10) + strings.Repeat("\x00", 11)), true},
		{"over-depth", func() thrift.FastCodec { return codec.NewCodecMessage() }, []byte(strings.Repeat("\x0c\x00\x63", 100) + strings.Repeat("\x00", 101)), false},
	} {
		t.Run(item.name, func(t *testing.T) {
			frame := make([]byte, 5, 5+len(item.payload))
			binary.BigEndian.PutUint32(frame[1:], uint32(len(item.payload)))
			frame = append(frame, item.payload...)
			err := grpcDecodePayload(item.newValue(), frame)
			if (err == nil) != item.accepted {
				t.Fatalf("accepted=%v want=%v err=%v payload=%x", err == nil, item.accepted, err, item.payload)
			}
			t.Logf("payload=%x accepted=%v error=%v", item.payload, err == nil, err)
		})
	}
}

func TestGRPCCodecReusesOrdinaryThriftGolden(t *testing.T) {
	file, err := makeGolden()
	if err != nil {
		t.Fatal(err)
	}
	for _, row := range file.Cases {
		t.Run(row.Name, func(t *testing.T) {
			payload, err := hex.DecodeString(row.Hex)
			if err != nil {
				t.Fatal(err)
			}
			var input, output thrift.FastCodec
			switch row.Type {
			case "CodecMessage":
				input, output = codec.NewCodecMessage(), codec.NewCodecMessage()
			case "ApplicationException":
				input, output = thrift.NewApplicationException(0, ""), thrift.NewApplicationException(0, "")
			default:
				t.Fatalf("unknown ordinary golden type %q", row.Type)
			}
			if _, err := input.FastRead(payload); err != nil {
				t.Fatal(err)
			}
			frame, err := grpcEncodePayload(input)
			if err != nil {
				t.Fatal(err)
			}
			if err := grpcDecodePayload(output, frame); err != nil {
				t.Fatal(err)
			}
			switch value := output.(type) {
			case *codec.CodecMessage:
				expected := row.Input
				if row.Name == "absent-default-fields" {
					// The gRPC codec materializes these unqualified default fields
					// when re-encoding a STOP-only struct. Keep optional fields and
					// every nonempty value strict; nil/empty default containers carry
					// the same Thrift value but different Go JSON presence.
					if len(value.Numbers) != 0 || len(value.Nested) != 0 || len(value.Ids) != 0 {
						t.Fatal("absent default containers must remain empty")
					}
					expected = make(map[string]any, len(row.Input))
					for key, value := range row.Input {
						expected[key] = value
					}
					actual := messageInput(value)
					for _, key := range []string{"numbers", "nested", "ids"} {
						expected[key] = actual[key]
					}
					t.Log("Kitex gRPC re-encoding materializes absent default containers; optional field presence is unchanged")
				}
				if !reflect.DeepEqual(messageInput(value), expected) {
					t.Fatalf("semantic mismatch: %+v", messageInput(value))
				}
			case *thrift.ApplicationException:
				if value.TypeId() != input.(*thrift.ApplicationException).TypeId() || value.Msg() != input.(*thrift.ApplicationException).Msg() {
					t.Fatal("application exception mismatch")
				}
			}
		})
	}
}

func TestGRPCIDLUnaryParameterMatrix(t *testing.T) {
	tool := os.Getenv("KITEX_GRPC_GENERATOR")
	if tool == "" {
		t.Skip("set KITEX_GRPC_GENERATOR to the fixed Kitex binary for generation verification")
	}
	for _, annotated := range []bool{false, true} {
		for count := 0; count <= 2; count++ {
			args := []string{}
			if count >= 1 {
				args = append(args, "1: Message a")
			}
			if count == 2 {
				args = append(args, "2: Message b")
			}
			annotation := ""
			if annotated {
				annotation = ` (streaming.mode="unary")`
			}
			directory := t.TempDir()
			idl := "namespace go matrix\nstruct Message {} service Matrix { Message Call(" + strings.Join(args, ",") + ")" + annotation + " }\n"
			if err := os.WriteFile(filepath.Join(directory, "input.thrift"), []byte(idl), 0644); err != nil {
				t.Fatal(err)
			}
			command := exec.Command(tool, "-module", "example.com/grpc-idl-matrix", "-streamx", "input.thrift")
			command.Dir = directory
			output, err := command.CombinedOutput()
			accepted := err == nil
			if accepted != (!annotated || count == 1) {
				t.Fatalf("annotated=%v args=%d: %v: %s", annotated, count, err, output)
			}
			t.Logf("annotated=%v args=%d accepted=%v", annotated, count, accepted)
		}
	}
}
