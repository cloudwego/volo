package oracle

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"reflect"
	"sort"

	"github.com/cloudwego/gopkg/protocol/thrift"
	codec "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/codec"
)

type goldenCase struct {
	Name             string         `json:"name"`
	Type             string         `json:"type"`
	Input            map[string]any `json:"input"`
	Hex              string         `json:"hex"`
	ExpectedConsumed int            `json:"expected_consumed"`
	CanonicalBytes   bool           `json:"canonical_bytes"`
}
type goldenFile struct {
	SchemaVersion int          `json:"schemaVersion"`
	Cases         []goldenCase `json:"cases"`
}

func stringPtr(s string) *string { return &s }

func messageInput(m *codec.CodecMessage) map[string]any {
	// Marshal finite values first; floats are always compared by IEEE bits.
	copy := *m
	copy.Real = 0
	b, err := json.Marshal(copy)
	if err != nil {
		panic(err)
	}
	var fields map[string]any
	// Preserve i64 boundaries exactly instead of converting to float64.
	decoder := json.NewDecoder(bytes.NewReader(b))
	decoder.UseNumber()
	if err := decoder.Decode(&fields); err != nil {
		panic(err)
	}
	delete(fields, "real")
	fields["real_bits"] = fmt.Sprintf("%016x", math.Float64bits(m.Real))
	fields["blob"] = hex.EncodeToString(m.Blob)
	return fields
}

func encoded(m thrift.FastCodec) []byte {
	b := make([]byte, m.BLength())
	n := m.FastWriteNocopy(b, nil)
	if n != len(b) {
		panic(fmt.Sprintf("size mismatch %d != %d", n, len(b)))
	}
	return b
}

func makeGolden() (goldenFile, error) {
	file := goldenFile{SchemaVersion: 1}
	add := func(name string, m *codec.CodecMessage, b []byte, canonical bool) {
		if b == nil {
			b = encoded(m)
		}
		decoded := codec.NewCodecMessage()
		n, err := decoded.FastRead(b)
		if err != nil || n != len(b) {
			panic(fmt.Sprintf("Go golden decode %s: %d/%d %v", name, n, len(b), err))
		}
		file.Cases = append(file.Cases, goldenCase{name, "CodecMessage", messageInput(decoded), hex.EncodeToString(b), n, canonical})
	}
	empty := codec.NewCodecMessage()
	add("empty-default", empty, nil, true)
	for _, item := range []struct {
		name  string
		value int64
	}{
		{"i64-min", math.MinInt64}, {"i64-max", math.MaxInt64}, {"i64-negative", -1},
	} {
		m := codec.NewCodecMessage()
		m.Integer = item.value
		add(item.name, m, nil, true)
	}
	for _, item := range []struct {
		name string
		bits uint64
	}{
		{"positive-zero", 0}, {"negative-zero", 1 << 63}, {"positive-infinity", 0x7ff0000000000000},
		{"negative-infinity", 0xfff0000000000000}, {"nan-payload", 0x7ff8000000000042},
		{"finite-double", math.Float64bits(1.2345678901234567)},
	} {
		m := codec.NewCodecMessage()
		m.Real = math.Float64frombits(item.bits)
		add(item.name, m, nil, true)
	}
	full := &codec.CodecMessage{
		Integer: math.MinInt64, Flag: true, Real: -42.5, Text: "hello\x00中文🦀", Blob: []byte{0, 1, 127, 128, 255},
		Numbers: []int32{math.MinInt32, -1, 0, math.MaxInt32}, Nested: map[string][]int64{"one": {math.MinInt64, 0, math.MaxInt64}},
		Note: stringPtr(""), Ids: []int16{math.MinInt16, -1, 0, math.MaxInt16},
		Child:    &codec.Nested{Value: math.MinInt32, Label: stringPtr("nested")},
		Children: []*codec.Nested{{Value: 0}, {Value: math.MaxInt32, Label: stringPtr("")}}, DefaultText: "changed",
	}
	// set ordering is not canonical across Rust/Go collections.
	add("full-nested", full, nil, false)
	full.Nested["two"] = []int64{}
	full.Nested[""] = []int64{-1}
	add("multi-map-set", full, nil, false)
	emptyCollections := codec.NewCodecMessage()
	emptyCollections.Numbers = []int32{}
	emptyCollections.Nested = map[string][]int64{}
	emptyCollections.Ids = []int16{}
	emptyCollections.Children = []*codec.Nested{}
	emptyCollections.Note = stringPtr("present")
	add("empty-present-optional", emptyCollections, nil, true)
	// Generate valid field spans using Go's binary codec, then reorder whole fields.
	b := encoded(empty)
	fields, err := fieldSpans(b)
	if err != nil {
		return file, err
	}
	reordered := []byte{}
	for i := len(fields) - 1; i >= 0; i-- {
		reordered = append(reordered, fields[i]...)
	}
	reordered = append(reordered, byte(thrift.STOP))
	add("reordered-fields", empty, reordered, false)
	unknown := make([]byte, thrift.Binary.FieldBeginLength()+thrift.Binary.StringLength("ignored"))
	n := thrift.Binary.WriteFieldBegin(unknown, thrift.STRING, 99)
	thrift.Binary.WriteString(unknown[n:], "ignored")
	unknown = append(unknown, b...)
	add("unknown-field", empty, unknown, false)
	// Default fields may be absent on the wire; the Go constructor supplies defaults.
	add("absent-default-fields", empty, []byte{byte(thrift.STOP)}, false)
	codes := []int32{1, 6, 7, 9, -1, math.MinInt32, math.MaxInt32, 456789}
	for code := int32(12001); code <= 12015; code++ {
		codes = append(codes, code)
	}
	for _, code := range codes {
		ex := thrift.NewApplicationException(code, "application 中文")
		b := encoded(ex)
		file.Cases = append(file.Cases, goldenCase{fmt.Sprintf("application-%d", code), "ApplicationException",
			map[string]any{"code": code, "message": ex.Msg()}, hex.EncodeToString(b), len(b), true})
	}
	return file, nil
}

func fieldSpans(b []byte) ([][]byte, error) {
	var fields [][]byte
	for offset := 0; offset < len(b); {
		t, _, n, err := thrift.Binary.ReadFieldBegin(b[offset:])
		if err != nil {
			return nil, err
		}
		if t == thrift.STOP {
			return fields, nil
		}
		payload, err := thrift.Binary.Skip(b[offset+n:], t)
		if err != nil {
			return nil, err
		}
		fields = append(fields, b[offset:offset+n+payload])
		offset += n + payload
	}
	return nil, fmt.Errorf("missing STOP")
}

func verifyGolden(path string) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var f goldenFile
	d := json.NewDecoder(bytes.NewReader(b))
	d.UseNumber()
	if err = d.Decode(&f); err != nil {
		return err
	}
	if f.SchemaVersion != 1 {
		return fmt.Errorf("unsupported schema %d", f.SchemaVersion)
	}
	if len(f.Cases) == 0 {
		return fmt.Errorf("empty fixture cannot verify compatibility")
	}
	for _, c := range f.Cases {
		b, err := hex.DecodeString(c.Hex)
		if err != nil {
			return err
		}
		var input map[string]any
		var n int
		switch c.Type {
		case "CodecMessage":
			m := codec.NewCodecMessage()
			n, err = m.FastRead(b)
			input = messageInput(m)
		case "ApplicationException":
			m := thrift.NewApplicationException(0, "")
			n, err = m.FastRead(b)
			input = map[string]any{"code": json.Number(fmt.Sprint(m.TypeId())), "message": m.Msg()}
		default:
			return fmt.Errorf("unknown fixture type %s", c.Type)
		}
		if err != nil {
			return fmt.Errorf("%s decode: %w", c.Name, err)
		}
		if n != len(b) || n != c.ExpectedConsumed {
			return fmt.Errorf("%s consumption %d/%d expected %d", c.Name, n, len(b), c.ExpectedConsumed)
		}
		// Thrift sets and maps have semantic equality, irrespective of write ordering.
		normalizeSet(input)
		normalizeSet(c.Input)
		if !reflect.DeepEqual(input, c.Input) {
			return fmt.Errorf("%s fields mismatch: got %v want %v", c.Name, input, c.Input)
		}
	}
	return nil
}

func normalizeSet(fields map[string]any) {
	if ids, ok := fields["ids"].([]any); ok {
		sort.Slice(ids, func(i, j int) bool { return fmt.Sprint(ids[i]) < fmt.Sprint(ids[j]) })
	}
}
