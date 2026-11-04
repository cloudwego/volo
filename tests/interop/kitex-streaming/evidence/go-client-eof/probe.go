package main

import (
	"context"
	"encoding/json"
	"errors"
	"github.com/cloudwego/gopkg/bufiox"
	"github.com/cloudwego/kitex/pkg/remote/trans/ttstream"
	"github.com/cloudwego/netpoll"
	"io"
	"os"
)

type failingReader struct {
	bufiox.Reader
	err error
}

func (r failingReader) Next(int) ([]byte, error) { return nil, r.err }
func main() {
	rows := []map[string]interface{}{}
	for _, item := range []struct {
		name string
		err  error
	}{{"netpoll-peer-close", netpoll.Exception(netpoll.ErrEOF, "peer close")}, {"io-eof", io.EOF}} {
		_, err := ttstream.DecodeFrame(context.Background(), failingReader{err: item.err})
		var typed interface{ TypeId() int32 }
		var code interface{}
		if errors.As(err, &typed) {
			code = typed.TypeId()
		}
		rows = append(rows, map[string]interface{}{"name": item.name, "source_is_io_eof": errors.Is(item.err, io.EOF), "source_is_netpoll_eof": errors.Is(item.err, netpoll.ErrEOF), "code": code, "error": err.Error()})
	}
	if err := json.NewEncoder(os.Stdout).Encode(rows); err != nil {
		panic(err)
	}
}
