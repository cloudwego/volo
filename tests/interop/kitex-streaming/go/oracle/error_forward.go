package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"sync"

	"github.com/cloudwego/kitex/pkg/kerrors"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type errorForwardServer struct {
	downstream service.Client
	node       string
}

var errorForwardState struct {
	sync.Mutex
	active int
}

func newErrorForwardServer(profile Profile, fixture serverFixture) (*errorForwardServer, error) {
	c, err := newTypedClient(profile, fixture.Forward, profile.DefaultPool)
	if err != nil {
		return nil, err
	}
	return &errorForwardServer{downstream: c, node: fixture.Node}, nil
}
func (h *errorForwardServer) bidi(ctx context.Context, stream message.Interop_BidiServer) (result error) {
	errorForwardState.Lock()
	errorForwardState.active++
	errorForwardState.Unlock()
	identity := ""
	defer func() {
		errorForwardState.Lock()
		defer errorForwardState.Unlock()
		errorForwardState.active--
		terminal := result
		if terminal == nil {
			terminal = io.EOF
		}
		event, _ := json.Marshal(map[string]any{"event": "error_forward_finish", "id": identity, "node": h.node, "terminal": observedErrorStatus(terminal), "active_handlers": errorForwardState.active})
		fmt.Println(string(event))
	}()
	first, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	var request errorStatusRequest
	if err = json.Unmarshal([]byte(first.Value), &request); err != nil {
		return err
	}
	identity = request.ID
	downstream, err := h.downstream.Bidi(ctx)
	if err != nil {
		return err
	}
	if err = downstream.Send(ctx, first); err != nil {
		return err
	}
	header, err := downstream.Header()
	if err != nil {
		return err
	}
	if err = stream.SetHeader(header); err != nil {
		return err
	}
	for {
		response, recvErr := downstream.Recv(ctx)
		_, biz := kerrors.FromBizStatusError(recvErr)
		// The Go typed API can return the actual DATA and a Biz error together.
		if recvErr == nil || (biz && response != nil && *response == *first) {
			if response == nil {
				return fmt.Errorf("nil downstream DATA")
			}
			if err = stream.Send(ctx, response); err != nil {
				return err
			}
		}
		if recvErr != nil {
			trailer, trailerErr := downstream.Trailer()
			if trailerErr != nil {
				return trailerErr
			}
			if err = stream.SetTrailer(trailer); err != nil {
				return err
			}
			if errors.Is(recvErr, io.EOF) {
				return nil
			}
			return recvErr
		}
	}
}
