package oracle

import (
	"context"
	"fmt"

	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
)

func payloadMatrix() ([]map[string]any, error) {
	scalar := int64(42)
	list := []*message.Message{{Value: "payload", Seq: 1}}
	mapValue := map[string]*message.Message{"k": {Value: "payload", Seq: 1}}
	set := []int32{1, 2}
	msg := &message.Message{Value: "payload", Seq: 1}
	var rows []map[string]any
	for _, c := range []struct {
		name     string
		value    any
		accepted bool
	}{
		{"struct", msg, true}, {"scalar", &scalar, false}, {"list", &list, false}, {"map", &mapValue, false}, {"set", &set, false},
	} {
		b, err := ttstream.EncodePayload(context.Background(), c.value)
		row := map[string]any{"name": c.name, "type": fmt.Sprintf("%T", c.value), "accepted": err == nil, "encoded_length": len(b), "expected_accepted": c.accepted}
		if err != nil {
			row["error"] = err.Error()
		}
		rows = append(rows, row)
		if (err == nil) != c.accepted {
			return rows, fmt.Errorf("%s unexpected payload encoder result", c.name)
		}
	}
	return rows, nil
}
