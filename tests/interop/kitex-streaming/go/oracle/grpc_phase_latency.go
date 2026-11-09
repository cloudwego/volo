package oracle

import (
	"context"
	"sort"
	"sync"
	"time"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/rpcinfo"
)

// Optional fixture instrumentation. Off does not install handlers or allocate a
// per-call recorder. Nothing is added to the wire or to the application payload.
type grpcPhaseKey struct{}
type grpcPhaseSample struct {
	header, first, last, finished int64
}
type grpcPhaseTiming struct {
	mu      sync.Mutex
	started time.Time
	value   grpcPhaseSample
	header  bool
	recvs   int
}

func phaseTiming(ctx context.Context) *grpcPhaseTiming {
	p, _ := ctx.Value(grpcPhaseKey{}).(*grpcPhaseTiming)
	return p
}
func (p *grpcPhaseTiming) receivedHeader() {
	if p == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.header {
		p.value.header = time.Since(p.started).Microseconds()
		p.header = true
	}
}
func (p *grpcPhaseTiming) receivedMessage() {
	if p == nil {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	now := time.Since(p.started).Microseconds()
	if p.recvs == 0 {
		p.value.first = now
	}
	p.value.last = now
	p.recvs++
}

func grpcPhaseOptions(mode string, enabled bool) []client.Option {
	if !enabled {
		return nil
	}
	events := rpcinfo.ClientStreamEventHandler{
		HandleStreamRecvHeaderEvent: func(ctx context.Context, _ rpcinfo.RPCInfo, _ rpcinfo.StreamRecvHeaderEvent) {
			phaseTiming(ctx).receivedHeader()
		},
	}
	if mode != "unary" {
		events.HandleStreamRecvEvent = func(ctx context.Context, _ rpcinfo.RPCInfo, event rpcinfo.StreamRecvEvent) {
			if event.Err == nil {
				phaseTiming(ctx).receivedMessage()
			}
		}
	}
	return []client.Option{client.WithStreamOptions(client.WithStreamEventHandler(events))}
}

type grpcPhaseRecorder struct {
	mu                                 sync.Mutex
	finished, errors, invalid, missing int
	values                             []grpcPhaseSample
}

func (r *grpcPhaseRecorder) finish(p *grpcPhaseTiming, err error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.value.finished = time.Since(p.started).Microseconds()
	r.mu.Lock()
	defer r.mu.Unlock()
	if !p.header || p.recvs == 0 {
		r.missing++
	}
	if p.value.header > p.value.first || p.value.first > p.value.last || p.value.last > p.value.finished {
		r.invalid++
	}
	if err != nil {
		r.errors++
	}
	if len(r.values) < cap(r.values) {
		r.values = append(r.values, p.value)
	} else {
		r.values[r.finished%len(r.values)] = p.value
	}
	r.finished++
}

func (r *grpcPhaseRecorder) summary() map[string]any {
	r.mu.Lock()
	defer r.mu.Unlock()
	histogram := func(get func(grpcPhaseSample) int64) map[string]any {
		values := make([]int64, len(r.values))
		for i, v := range r.values {
			values[i] = get(v)
		}
		sort.Slice(values, func(a, b int) bool { return values[a] < values[b] })
		var p50, p99 any
		if len(values) > 0 {
			p50, p99 = values[(len(values)-1)*50/100], values[(len(values)-1)*99/100]
		}
		return map[string]any{"count": len(values), "p50": p50, "p99": p99}
	}
	return map[string]any{
		"scope":  "Go H2 Header and StreamX receive callbacks; unary decoded response at call return; clock and locking overhead included",
		"origin": "application benchmark call start after warmup", "sample_limit": cap(r.values), "sample_count": len(r.values),
		"finished": r.finished, "errors": r.errors, "invalid_order": r.invalid, "missing_callbacks": r.missing,
		"header_us":        histogram(func(v grpcPhaseSample) int64 { return v.header }),
		"first_message_us": histogram(func(v grpcPhaseSample) int64 { return v.first }),
		"last_message_us":  histogram(func(v grpcPhaseSample) int64 { return v.last }),
		"finished_us":      histogram(func(v grpcPhaseSample) int64 { return v.finished }),
	}
}
