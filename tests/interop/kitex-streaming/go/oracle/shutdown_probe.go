package oracle

import (
	"bufio"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"os"
	"time"

	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

func writeAll(conn net.Conn, value []byte) error {
	for len(value) > 0 {
		n, err := conn.Write(value)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		value = value[n:]
	}
	return nil
}

func receiveWireFrames(conn net.Conn, sid int32, maximum int) ([]map[string]any, error) {
	var frames []map[string]any
	for i := 0; i < maximum; i++ {
		var prefix [4]byte
		if _, err := io.ReadFull(conn, prefix[:]); err != nil {
			return frames, err
		}
		length := binary.BigEndian.Uint32(prefix[:])
		if length < 10 || length > 1024*1024 {
			return frames, fmt.Errorf("unbounded response %d", length)
		}
		bytes := make([]byte, int(length)+4)
		copy(bytes, prefix[:])
		if _, err := io.ReadFull(conn, bytes[4:]); err != nil {
			return frames, err
		}
		frame, err := decodeFrame(bytes)
		if err != nil {
			return frames, err
		}
		frame["hex"] = hex.EncodeToString(bytes)
		frames = append(frames, frame)
		if frame["sid"].(int32) != sid {
			return frames, fmt.Errorf("foreign sid in shutdown response")
		}
		kind := frame["int_metadata"].(map[uint16]string)[27]
		if kind == "4" || kind == "5" {
			break
		}
	}
	return frames, nil
}

// shutdownProbe retains one unary and one TTStream connection across SIGTERM.
// The runner releases its barrier only after the real generated server's
// shutdown hook runs. A bounded series allows the tiny hook-to-mark scheduling
// window, while exposing the previous two-second streaming admission delay.
func shutdownProbe(profile Profile, addr, report string) error {
	network, target, err := networkAddress(addr)
	if err != nil {
		return err
	}
	unary, err := net.DialTimeout(network, target, 5*time.Second)
	if err != nil {
		return err
	}
	defer unary.Close()
	stream, err := net.DialTimeout(network, target, 5*time.Second)
	if err != nil {
		return err
	}
	defer stream.Close()
	result := map[string]any{"event": "result", "api": "shutdown-wire", "profile": profile.Name,
		"unary_dials": 1, "stream_dials": 1, "unary_local_addr": unary.LocalAddr().String(), "stream_local_addr": stream.LocalAddr().String()}
	var phases []map[string]any
	var sent []map[string]any
	unaryCall := func(seq int32) error {
		if err := unary.SetDeadline(time.Now().Add(time.Second)); err != nil {
			return err
		}
		request := &message.Message{Value: "shutdown-unary", Seq: seq}
		bytes, err := unaryRequest(seq, request)
		if err != nil {
			return err
		}
		if err = writeAll(unary, bytes); err != nil {
			return err
		}
		reply, err := unaryReply(unary, seq, request)
		phases = append(phases, map[string]any{"seq": seq, "request_hex": hex.EncodeToString(bytes), "response_hex": hex.EncodeToString(reply), "ok": err == nil})
		return err
	}
	sendFrame := func(sid int32, kind int, action, identity string) error {
		spec := frameSpec{SID: sid, Kind: kind, Method: "Bidi"}
		if kind == 2 {
			spec.Metadata = map[string]string{"isn": "Interop"}
		}
		if kind == 3 {
			value, err := lifecycleMessage(action, identity)
			if err != nil {
				return err
			}
			spec.PayloadHex = hex.EncodeToString(encoded(value))
		}
		bytes, err := frameBytes(spec)
		if err != nil {
			return err
		}
		sent = append(sent, map[string]any{"sid": sid, "kind": kind, "hex": hex.EncodeToString(bytes)})
		return writeAll(stream, bytes)
	}
	run := func() error {
		if err = unaryCall(1); err != nil {
			return err
		}
		if err = stream.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
			return err
		}
		if err = sendFrame(1, 2, "", ""); err != nil {
			return err
		}
		if err = sendFrame(1, 3, "start", "shutdown-live"); err != nil {
			return err
		}
		ack, readErr := receiveWireFrames(stream, 1, 2)
		result["ack_frames"] = ack
		if readErr != nil || len(ack) != 2 {
			return fmt.Errorf("shutdown live ACK: %v", readErr)
		}
		if value, ok := ack[1]["message"].(*message.Message); !ok || !lifecycleAction(value, "ack", "shutdown-live") {
			return fmt.Errorf("shutdown live ACK identity differs")
		}
		if err = responseEvent(map[string]any{"event": "barrier", "id": "shutdown-live"}); err != nil {
			return err
		}
		line, readErr := bufio.NewReader(os.Stdin).ReadString('\n')
		if readErr != nil || line != "continue\n" {
			return fmt.Errorf("shutdown explicit barrier: %v", readErr)
		}
		started := time.Now()
		if err = unaryCall(2); err != nil {
			return err
		}
		var probes []map[string]any
		rejected := false
		for sid := int32(2); sid <= 65; sid++ {
			if err = stream.SetDeadline(time.Now().Add(time.Second)); err != nil {
				return err
			}
			if err = sendFrame(sid, 2, "", ""); err != nil {
				return err
			}
			if err = sendFrame(sid, 3, "probe", fmt.Sprintf("shutdown-probe-%d", sid)); err != nil {
				return err
			}
			if err = sendFrame(sid, 4, "", ""); err != nil {
				return err
			}
			frames, readErr := receiveWireFrames(stream, sid, 4)
			probes = append(probes, map[string]any{"sid": sid, "frames": frames, "elapsed_ms": time.Since(started).Milliseconds()})
			result["probes"] = probes
			if readErr != nil || len(frames) == 0 {
				return fmt.Errorf("shutdown admission probe: %v", readErr)
			}
			if frames[len(frames)-1]["int_metadata"].(map[uint16]string)[27] == "5" {
				rejected = true
				break
			}
		}
		result["rejected_new_stream"], result["admission_elapsed_ms"] = rejected, time.Since(started).Milliseconds()
		if err = sendFrame(1, 3, "release", "shutdown-live"); err != nil {
			return err
		}
		if err = sendFrame(1, 4, "", ""); err != nil {
			return err
		}
		frames, readErr := receiveWireFrames(stream, 1, 3)
		result["live_finish_frames"] = frames
		return readErr
	}
	err = run()
	result["unary_phases"], result["sent"], result["ok"] = phases, sent, err == nil
	if err != nil {
		result["error"] = err.Error()
	}
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	return err
}
