package oracle

import (
	"context"
	"fmt"
	"io"
	"strings"
	"time"

	grpcstatus "github.com/cloudwego/kitex/pkg/remote/trans/nphttp2/status"
	parent "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/parent"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/grpc/payload/api/interop"
)

// The probe uses the actual generated StreamX or legacy transport. The adapter
// only supplies an observer of its SDK state; it cannot manufacture wire events.
func grpcMetadataProbe(profile Profile, address, scenario, report string) error {
	if profile.SnapshotMetadata == nil {
		return fmt.Errorf("profile does not provide a metadata observer")
	}
	legacy := strings.HasPrefix(scenario, "legacy-")
	explicit := strings.HasSuffix(scenario, "-explicit")
	if scenario != "streamx-explicit" && scenario != "streamx-implicit" &&
		scenario != "legacy-explicit" && scenario != "legacy-implicit" {
		return fmt.Errorf("unknown metadata scenario %q", scenario)
	}
	var client service.Client
	var err error
	if legacy {
		client, err = newLegacyGRPCClient(profile, address, "identity")
	} else {
		client, err = newGRPCClient(profile, address)
	}
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	stream, err := client.Bidi(ctx)
	if err != nil {
		return err
	}
	type snapshot struct {
		Stage string            `json:"stage"`
		State map[string]string `json:"state"`
	}
	var snapshots []snapshot
	capture := func(stage string) {
		snapshots = append(snapshots, snapshot{stage, profile.SnapshotMetadata(stream.Context())})
	}
	capture("open")
	if err = stream.Send(ctx, &parent.Message{}); err != nil {
		return err
	}
	if err = stream.CloseSend(ctx); err != nil {
		return err
	}
	var headers map[string]string
	var headerErr error
	if explicit {
		headers, headerErr = stream.Header()
		capture("header")
	}
	count := 0
	for count < 10 {
		_, err = stream.Recv(ctx)
		if err != nil {
			break
		}
		count++
		capture("message")
	}
	if err == nil {
		return fmt.Errorf("metadata response exceeded probe message budget")
	}
	capture("terminal")
	trailers, trailerErr := stream.Trailer()
	capture("trailer")
	code := int(grpcstatus.Code(err))
	if err == io.EOF {
		code = 0
	}
	return writeJSON(report, map[string]any{"schemaVersion": 1, "event": "result", "status": "OBSERVED",
		"api": scenario, "code": code, "messages": count, "headers": headers, "header_error": fmt.Sprint(headerErr), "trailers": trailers,
		"snapshots": snapshots, "actual_error": fmt.Sprint(err), "trailer_error": fmt.Sprint(trailerErr)})
}
