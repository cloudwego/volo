package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/cloudwego/gopkg/protocol/thrift"
	"github.com/cloudwego/kitex/pkg/kerrors"
	"github.com/cloudwego/kitex/pkg/streaming"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type errorStatusRequest struct {
	SchemaVersion int               `json:"schemaVersion"`
	ID            string            `json:"id"`
	Kind          string            `json:"kind"`
	Code          int32             `json:"code"`
	Message       string            `json:"message"`
	Extra         map[string]string `json:"extra"`
}

func errorStatusBidi(ctx context.Context, stream message.Interop_BidiServer) error {
	first, err := stream.Recv(ctx)
	if err != nil {
		return err
	}
	var request errorStatusRequest
	if err := json.Unmarshal([]byte(first.Value), &request); err != nil {
		return err
	}
	if request.SchemaVersion != 1 || request.ID == "" || first.Seq != 1 {
		return fmt.Errorf("invalid error-status request")
	}
	if request.Kind != "biz" && request.Kind != "application" && request.Kind != "ok" {
		return fmt.Errorf("invalid error-status kind %q", request.Kind)
	}
	if err := stream.SetHeader(streaming.Header{"interop-error-header": "header-value"}); err != nil {
		return err
	}
	if err := stream.SetTrailer(streaming.Trailer{"interop-error-trailer": "trailer-value"}); err != nil {
		return err
	}
	if err := stream.Send(ctx, first); err != nil {
		return err
	}
	switch request.Kind {
	case "biz":
		return kerrors.NewBizStatusErrorWithExtra(request.Code, request.Message, request.Extra)
	case "application":
		return thrift.NewApplicationException(request.Code, request.Message)
	default:
		return nil
	}
}

func observedErrorStatus(err error) map[string]any {
	result := map[string]any{"kind": "unknown", "code": nil, "message": "", "extra": map[string]string{}, "kitex_code": nil, "error": nil}
	if errors.Is(err, io.EOF) {
		result["kind"], result["code"], result["kitex_code"] = "ok", int32(0), int32(0)
		return result
	}
	if err == nil {
		result["error"] = "missing terminal error/EOF"
		return result
	}
	result["error"] = err.Error()
	var typed interface{ TypeId() int32 }
	if errors.As(err, &typed) {
		result["kitex_code"] = typed.TypeId()
	}
	if biz, ok := kerrors.FromBizStatusError(err); ok {
		result["kind"], result["code"], result["message"], result["extra"] = "biz", biz.BizStatusCode(), biz.BizMessage(), biz.BizExtra()
		return result
	}
	var app *thrift.ApplicationException
	if errors.As(err, &app) {
		result["kind"], result["code"], result["message"] = "application", app.TypeID(), app.Msg()
		result["application_code_available"] = true
		return result
	}
	if result["kitex_code"] == int32(12001) {
		// Kitex ttstream.Exception does not expose its cause through Unwrap/As.
		// Its public Trailer only exposes metadata, not the exception body.
		// Preserve actual evidence; never copy the expected request code here.
		result["kind"] = "application"
		result["message"] = err.Error()
		result["application_code_available"] = false
		result["observation"] = "application payload code unavailable through Kitex typed API; inspect wire fixture"
	}
	return result
}

func errorStatusCall(profile Profile, addr, pool, mode, identity, report string, recvDelay time.Duration) error {
	if mode != "biz" && mode != "application" && mode != "ok" {
		return fmt.Errorf("invalid error-status mode %q", mode)
	}
	c, err := newTypedClient(profile, addr, pool)
	if err != nil {
		return err
	}
	run := func(kind, id string) ([]*message.Message, map[string]any, streaming.Header, streaming.Trailer, error) {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		code := int32(0)
		if kind == "biz" {
			code = 4123
		} else if kind == "application" {
			code = 6
		}
		request := errorStatusRequest{SchemaVersion: 1, ID: id, Kind: kind, Code: code, Message: "fixture-terminal", Extra: map[string]string{"detail": "interop-中文"}}
		payload, err := json.Marshal(request)
		if err != nil {
			return nil, nil, nil, nil, err
		}
		var stream service.Interop_BidiClient
		stream, err = c.Bidi(ctx)
		if err != nil {
			return nil, nil, nil, nil, err
		}
		first := &message.Message{Value: string(payload), Seq: 1}
		if err := stream.Send(ctx, first); err != nil {
			return nil, nil, nil, nil, err
		}
		if recvDelay > 0 {
			// Intentional application consumption delay for the Go reference race,
			// not a readiness delay or a production transport workaround.
			timer := time.NewTimer(recvDelay)
			select {
			case <-timer.C:
			case <-ctx.Done():
				timer.Stop()
				return nil, nil, nil, nil, ctx.Err()
			}
		}
		response, err := stream.Recv(ctx)
		_, bizWithData := kerrors.FromBizStatusError(err)
		if err != nil && !(bizWithData && response != nil && *response == *first) {
			return nil, nil, nil, nil, fmt.Errorf("error-status missing echo DATA: %w", err)
		}
		if *response != *first {
			return nil, nil, nil, nil, fmt.Errorf("error-status echo differs: %v", response)
		}
		terminalErr := err
		if terminalErr == nil {
			extra, end := stream.Recv(ctx)
			terminalErr = end
			if terminalErr == nil {
				return nil, nil, nil, nil, fmt.Errorf("error-status extra DATA: %v", extra)
			}
		}
		terminal := observedErrorStatus(terminalErr)
		// Kitex client/stream.go can replace a successful DATA decode's nil
		// error with the already observed BizStatusErr. Preserve both actual
		// return values and label the API difference instead of inventing a
		// second Recv or hiding the DATA from this report.
		terminal["data_and_error_same_recv"] = bizWithData
		header, headerErr := stream.Header()
		trailer, trailerErr := stream.Trailer()
		if headerErr != nil || trailerErr != nil {
			return []*message.Message{response}, terminal, header, trailer, fmt.Errorf("error-status metadata: header=%v trailer=%v", headerErr, trailerErr)
		}
		return []*message.Message{response}, terminal, header, trailer, nil
	}
	responses, terminal, header, trailer, callErr := run(mode, identity)
	_, probeTerminal, _, _, probeErr := run("ok", identity+"-probe")
	probeOK := probeErr == nil && probeTerminal["kind"] == "ok"
	valid := callErr == nil && terminal["kind"] == mode
	if valid {
		switch mode {
		case "biz":
			bizExtra, ok := terminal["extra"].(map[string]string)
			valid = terminal["code"] == int32(4123) && terminal["message"] == "fixture-terminal" && ok && bizExtra["detail"] == "interop-中文"
		case "application":
			// Payload code is checked only if the API exposes an actual application cause.
			if available, _ := terminal["application_code_available"].(bool); available {
				valid = terminal["code"] == int32(6) && terminal["message"] == "fixture-terminal"
			} else {
				valid = terminal["kitex_code"] == int32(12001) && strings.Contains(fmt.Sprint(terminal["error"]), "fixture-terminal")
			}
		}
	}
	result := map[string]any{"event": "result", "schemaVersion": 1, "api": "errors", "profile": profile.Name, "kitex_module": profile.KitexModule, "id": identity, "mode": mode, "pool": pool, "recv_delay_ms": recvDelay.Milliseconds(), "responses": responses, "terminal": terminal, "header": header, "trailer": trailer, "probe_ok": probeOK, "ok": valid && probeOK}
	if callErr != nil {
		result["error"] = callErr.Error()
	}
	if probeErr != nil {
		result["probe_error"] = probeErr.Error()
	}
	if err := writeJSON(report, result); err != nil {
		return err
	}
	if !valid || !probeOK {
		return fmt.Errorf("error-status observation or recovery check failed")
	}
	return nil
}
