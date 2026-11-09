package oracle

import (
	"bufio"
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
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

// WireAction bypasses typed request cardinality to exercise the real frame
// decoder and connection state machine. Encoding still uses Go's TTHeader and
// generated Thrift codecs; this is not a reimplementation of their wire format.
type WireAction struct {
	Op            string           `json:"op"`
	Frame         frameSpec        `json:"frame"`
	Message       *message.Message `json:"message,omitempty"`
	Exception     *WireException   `json:"exception,omitempty"`
	UntilSID      *int32           `json:"until_sid,omitempty"`
	MaxFrames     int              `json:"max_frames,omitempty"`
	TimeoutMS     int              `json:"timeout_ms,omitempty"`
	AllowTimeout  bool             `json:"allow_timeout,omitempty"`
	FragmentBytes int              `json:"fragment_bytes,omitempty"`
	PayloadSuffix string           `json:"payload_suffix_hex,omitempty"`
}

type WireException struct {
	Code    int32  `json:"code"`
	Message string `json:"message"`
}

type WireScenario struct {
	SchemaVersion int          `json:"schemaVersion"`
	Actions       []WireAction `json:"actions"`
}

func wire(addr, input, report string) error {
	data, err := os.ReadFile(input)
	if err != nil {
		return err
	}
	var scenario WireScenario
	if err = json.Unmarshal(data, &scenario); err != nil {
		return err
	}
	if scenario.SchemaVersion != 1 || len(scenario.Actions) == 0 {
		return fmt.Errorf("invalid or empty wire scenario")
	}
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	conn, err := net.DialTimeout(network, target, 5*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	var observed []map[string]any
	result := map[string]any{"schemaVersion": 1, "event": "result", "api": "raw-wire", "addr": addr, "local_addr": conn.LocalAddr().String()}
	for index, action := range scenario.Actions {
		row := map[string]any{"index": index, "op": action.Op}
		observed = append(observed, row)
		timeout := time.Second
		if action.TimeoutMS > 0 {
			timeout = time.Duration(action.TimeoutMS) * time.Millisecond
		}
		if err = conn.SetDeadline(time.Now().Add(timeout)); err != nil {
			break
		}
		switch action.Op {
		case "barrier":
			// Explicit runner synchronization, used to drain a server only after
			// this same socket has received its first live-stream response.
			if err = responseEvent(map[string]any{"event": "barrier", "index": index}); err != nil {
				break
			}
			var line string
			line, err = bufio.NewReader(os.Stdin).ReadString('\n')
			if err == nil && line != "continue\n" {
				err = fmt.Errorf("wire barrier requires an explicit continue line")
			}
			row["continued"] = err == nil
		case "close":
			err = conn.Close()
			row["closed"] = err == nil
		case "send":
			if action.Message != nil && action.Exception != nil {
				err = fmt.Errorf("message and exception are mutually exclusive")
				break
			}
			if action.Message != nil {
				action.Frame.PayloadHex = hex.EncodeToString(encoded(action.Message))
			}
			if action.Exception != nil {
				var payload []byte
				payload, err = ttstream.EncodeException(context.Background(), action.Frame.Method, action.Frame.SID,
					thrift.NewApplicationException(action.Exception.Code, action.Exception.Message))
				if err != nil {
					break
				}
				action.Frame.PayloadHex = hex.EncodeToString(payload)
			}
			action.Frame.PayloadHex += action.PayloadSuffix
			var frame []byte
			frame, err = frameBytes(action.Frame)
			if err != nil {
				break
			}
			row["hex"] = hex.EncodeToString(frame)
			for offset := 0; offset < len(frame); {
				end := len(frame)
				if action.FragmentBytes > 0 && offset+action.FragmentBytes < end {
					end = offset + action.FragmentBytes
				}
				var n int
				n, err = conn.Write(frame[offset:end])
				if err != nil {
					break
				}
				if n == 0 {
					err = io.ErrShortWrite
					break
				}
				offset += n
			}
		case "read":
			var frames []map[string]any
			maximum := action.MaxFrames
			if maximum <= 0 {
				maximum = 64
			}
			terminal := false
			for i := 0; i < maximum; i++ {
				var prefix [4]byte
				_, err = io.ReadFull(conn, prefix[:])
				if err != nil {
					break
				}
				length := binary.BigEndian.Uint32(prefix[:])
				if length < 10 || length > 16*1024*1024 {
					err = fmt.Errorf("unbounded/invalid response frame length %d", length)
					break
				}
				b := make([]byte, int(length)+4)
				copy(b, prefix[:])
				_, err = io.ReadFull(conn, b[4:])
				if err != nil {
					break
				}
				var decoded map[string]any
				decoded, err = decodeFrame(b)
				if err != nil {
					break
				}
				decoded["hex"] = hex.EncodeToString(b)
				frames = append(frames, decoded)
				if action.UntilSID != nil {
					meta := decoded["int_metadata"].(map[uint16]string)
					if decoded["sid"].(int32) == *action.UntilSID && (meta[27] == "4" || meta[27] == "5") {
						terminal = true
						break
					}
				}
			}
			row["frames"] = frames
			row["terminal"] = terminal
			if err != nil {
				if timeoutErr, ok := err.(net.Error); ok && timeoutErr.Timeout() && action.AllowTimeout {
					row["timed_out"] = true
					err = nil
				}
			} else if action.UntilSID != nil && !terminal {
				err = fmt.Errorf("no terminal for sid %d within frame limit", *action.UntilSID)
			}
		default:
			err = fmt.Errorf("unsupported wire action %q", action.Op)
		}
		if err != nil {
			row["error"] = err.Error()
			break
		}
	}
	result["actions"] = observed
	result["ok"] = err == nil
	if err != nil {
		result["error"] = err.Error()
	}
	if e := writeJSON(report, result); e != nil {
		return e
	}
	return err
}
