package oracle

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/bytedance/gopkg/cloud/metainfo"
	"github.com/cloudwego/kitex/pkg/transmeta"
	message "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop"
	service "github.com/cloudwego/volo/tests/interop/kitexstreaming/kitex_gen/interop/interop"
)

type serverFixture struct {
	API            string
	Scenario       string
	Node           string
	Forward        string
	MetainfoMode   string
	RoutingProfile string
}

type metaObservation struct {
	Node         string            `json:"node"`
	Persistent   map[string]string `json:"persistent"`
	Transient    map[string]string `json:"transient"`
	HasDeadline  bool              `json:"has_deadline"`
	HeaderString map[string]string `json:"header_string"`
	HeaderInt    map[uint16]string `json:"header_int"`
}

type metaRequest struct {
	SchemaVersion  int               `json:"schemaVersion"`
	ID             string            `json:"id"`
	ResetTransient map[string]string `json:"reset_transient"`
	SetPersistent  map[string]string `json:"set_persistent"`
	Observations   []metaObservation `json:"observations"`
}

type metaSeed struct {
	Persistent map[string]string `json:"persistent"`
	Transient  map[string]string `json:"transient"`
	Request    metaRequest       `json:"request"`
}

type metadataServer struct {
	node       string
	downstream service.Client
}

func setMetainfoFixtureMode(mode string) error {
	if mode == "default" {
		return nil // Preserve the actual profile default/provider, including custom defaults.
	}
	var selected transmeta.MetainfoPropagationMode
	switch mode {
	case "single-hop":
		selected = transmeta.MetainfoPropagationSingleHop
	case "legacy":
		selected = transmeta.MetainfoPropagationLegacy
	default:
		return fmt.Errorf("invalid metainfo fixture mode %q", mode)
	}
	// Fixture-only override installed after profile initialization. The production
	// implementation is not edited and Rust never gets a legacy-mode switch.
	transmeta.SetMetainfoPropagationModeProvider(func(context.Context) transmeta.MetainfoPropagationMode { return selected })
	return nil
}

func newMetadataServer(profile Profile, fixture serverFixture) (*metadataServer, error) {
	h := &metadataServer{node: fixture.Node}
	if fixture.Forward != "" {
		var err error
		h.downstream, err = newTypedClient(profile, fixture.Forward, profile.DefaultPool)
		if err != nil {
			return nil, err
		}
	}
	return h, nil
}

func fixtureValues(values map[string]string) map[string]string {
	result := map[string]string{}
	for key, value := range values {
		if strings.HasPrefix(key, "interop-") {
			result[key] = value
		}
	}
	return result
}

func (h *metadataServer) bidi(ctx context.Context, stream message.Interop_BidiServer) error {
	for {
		m, err := stream.Recv(ctx)
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		var request metaRequest
		if err = json.Unmarshal([]byte(m.Value), &request); err != nil {
			return err
		}
		if request.SchemaVersion != 1 || request.ID == "" {
			return fmt.Errorf("invalid metainfo request")
		}
		_, hasDeadline := ctx.Deadline()
		request.Observations = append(request.Observations, metaObservation{
			Node: h.node, Persistent: fixtureValues(metainfo.GetAllPersistentValues(ctx)),
			Transient: fixtureValues(metainfo.GetAllValues(ctx)), HasDeadline: hasDeadline,
			// ServerStream has no incoming raw HEADER getter. Null is honest absence
			// of wire evidence; receiver-visible metainfo is observed from real ctx.
			HeaderString: nil, HeaderInt: nil,
		})
		outbound := ctx
		if h.downstream != nil {
			for key, value := range request.ResetTransient {
				outbound = metainfo.WithValue(outbound, key, value)
			}
			for key, value := range request.SetPersistent {
				outbound = metainfo.WithPersistentValue(outbound, key, value)
			}
		}
		payload, err := json.Marshal(request)
		if err != nil {
			return err
		}
		response := &message.Message{Value: string(payload), Seq: m.Seq}
		if h.downstream != nil {
			downstream, err := h.downstream.Bidi(outbound)
			if err != nil {
				return err
			}
			if err = downstream.Send(outbound, response); err != nil {
				return err
			}
			if err = downstream.CloseSend(outbound); err != nil {
				return err
			}
			response, err = downstream.Recv(outbound)
			if err != nil {
				return err
			}
			if _, end := downstream.Recv(outbound); !errors.Is(end, io.EOF) {
				return fmt.Errorf("downstream did not finish after its observation: %v", end)
			}
		}
		if err = stream.Send(ctx, response); err != nil {
			return err
		}
	}
}

func metadataCall(profile Profile, addr, pool, input, report string, batch bool) error {
	encoded, err := os.ReadFile(input)
	if err != nil {
		return err
	}
	var seeds []metaSeed
	if batch {
		if err = json.Unmarshal(encoded, &seeds); err != nil {
			return err
		}
	} else {
		var seed metaSeed
		if err = json.Unmarshal(encoded, &seed); err != nil {
			return err
		}
		seeds = []metaSeed{seed}
	}
	if len(seeds) == 0 || len(seeds) > 32 {
		return fmt.Errorf("metainfo fixture requires 1..32 seeds")
	}
	c, err := newTypedClient(profile, addr, pool)
	if err != nil {
		return err
	}
	results := make([]map[string]any, len(seeds))
	failures := make([]error, len(seeds))
	var workers sync.WaitGroup
	for index, seed := range seeds {
		workers.Add(1)
		go func(index int, seed metaSeed) {
			defer workers.Done()
			results[index], failures[index] = metadataExchange(profile, c, seed)
			if failures[index] != nil {
				results[index] = map[string]any{"id": seed.Request.ID, "ok": false, "error": failures[index].Error()}
			}
		}(index, seed)
	}
	workers.Wait()
	for _, failure := range failures {
		if failure != nil {
			err = failure
			break
		}
	}
	var result any = results[0]
	if batch {
		result = map[string]any{"event": "result", "ok": err == nil, "api": "metainfo-batch",
			"profile": profile.Name, "kitex_module": profile.KitexModule, "pool": pool, "results": results}
	}
	if writeErr := writeJSON(report, result); writeErr != nil {
		return writeErr
	}
	return err
}

func metadataExchange(profile Profile, c service.Client, seed metaSeed) (map[string]any, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for key, value := range seed.Persistent {
		ctx = metainfo.WithPersistentValue(ctx, key, value)
	}
	for key, value := range seed.Transient {
		ctx = metainfo.WithValue(ctx, key, value)
	}
	stream, err := c.Bidi(ctx)
	if err != nil {
		return nil, err
	}
	payload, err := json.Marshal(seed.Request)
	if err != nil {
		return nil, err
	}
	if err = stream.Send(ctx, &message.Message{Value: string(payload), Seq: 1}); err != nil {
		return nil, err
	}
	if err = stream.CloseSend(ctx); err != nil {
		return nil, err
	}
	response, err := stream.Recv(ctx)
	if err != nil {
		return nil, err
	}
	if _, end := stream.Recv(ctx); !errors.Is(end, io.EOF) {
		return nil, fmt.Errorf("metainfo response did not finish: %v", end)
	}
	var observed metaRequest
	if err = json.Unmarshal([]byte(response.Value), &observed); err != nil {
		return nil, err
	}
	if observed.ID != seed.Request.ID || observed.SchemaVersion != 1 || response.Seq != 1 {
		return nil, fmt.Errorf("metainfo response identity differs")
	}
	return map[string]any{"event": "result", "ok": true, "api": "metainfo", "profile": profile.Name,
		"kitex_module": profile.KitexModule, "id": observed.ID, "observations": observed.Observations,
		"client_local_timeout_seconds": 10}, nil
}
