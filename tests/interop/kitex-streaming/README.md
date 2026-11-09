# Kitex streaming interoperability tools

This directory owns the shared Python runner, fixed upstream Go module, generated
Go codecs and network scenarios. Pilota consumes committed golden files.
Additional consumers can supply initialization options through `oracle.Profile`.
Do not fork the process manager or copy the network scenarios into another repository.

The canonical IDLs live in `fixtures/interop.thrift` and `go/idl/audit*.thrift`.
Rust consumers use the [shared fixture helper](rust-fixtures/README.md);
its generated Rust view preserves the existing requiredness without changing the Go source.
The standalone helper crate is test support and is not published.

The candidate tool version is 0.1.0 and report schema is 1. The Go dependency is
`github.com/cloudwego/kitex v0.16.4`; that module's generator currently identifies
itself as v0.16.3 in generated comments. Preserve this upstream fact and identify
the actual source with go.mod/go.sum and binary SHA256, rather than editing comments.

Build from this directory:

```sh
python3 -m pip install .
cd go
go build -buildvcs=false -mod=readonly -o ../bin/kitex github.com/cloudwego/kitex/tool/cmd/kitex
go build -buildvcs=false -mod=readonly -o ../bin/oracle ./cmd/oracle
```

The Python package supports Python 3.7+; Python <3.11 uses the declared `tomli`
dependency for profile.toml. Without installation, use `PYTHONPATH=python` from
this directory. The Go toolchain used for the first evidence was Go 1.24.11.

Implemented commands (all binaries are explicit and reports include their hashes):

```sh
python3 -m kitex_streaming_interop --profile upstream --suite idl-matrix --kitex-bin bin/kitex --idl-root /path/to/pilota/tests/interop/kitex-codec --out target/idl
python3 -m kitex_streaming_interop --profile upstream --suite codec-golden --go-bin bin/oracle --fixture-root /path/to/pilota/tests/interop/kitex-codec --rust-bytes /path/to/rust-golden.json --out target/codec
python3 -m kitex_streaming_interop --profile upstream --suite codec-golden --go-bin bin/oracle --regenerate --out target/proposed-golden
python3 -m kitex_streaming_interop --profile upstream --suite raw-basic --go-bin bin/oracle --rust-bin /path/to/ttstream_interop --out target/raw
python3 -m kitex_streaming_interop --profile upstream --suite typed-basic --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --pool short,long,mux --out target/typed
python3 -m kitex_streaming_interop --profile upstream --suite wire-boundaries --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/wire
python3 -m kitex_streaming_interop --profile upstream --suite idle-unary --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --idle-seconds 300 --timeout 640 --out target/idle
python3 -m kitex_streaming_interop --profile upstream --suite metainfo --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --metainfo-concurrency 16 --out target/metainfo
python3 -m kitex_streaming_interop --profile upstream --suite cascade --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/cascade
python3 -m kitex_streaming_interop --profile upstream --suite typed-basic --go-api binary --mode client,server,bidi --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/go-binary-generic
python3 -m kitex_streaming_interop --profile upstream --suite error-status --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/errors
python3 -m kitex_streaming_interop --profile upstream --suite response-boundaries --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/responses
python3 -m kitex_streaming_interop --profile upstream --suite service-routing --go-bin bin/oracle --rust-bin /path/to/ttstream-codegen --out target/routing
python3 -m kitex_streaming_interop --profile upstream --suite timeout-policy,drain-disconnect --go-bin bin/oracle --rust-bin /path/to/ttstream-lifecycle --out target/lifecycle
python3 -m kitex_streaming_interop --profile upstream --suite server-shutdown --go-bin bin/oracle --rust-bin /path/to/ttstream-lifecycle --out target/shutdown
python3 -m kitex_streaming_interop --profile upstream --suite budget-limits --go-bin bin/oracle --rust-bin /path/to/ttstream-lifecycle --out target/budget
python3 -m kitex_streaming_interop --profile upstream --suite inherited-service --go-bin bin/oracle --rust-bin /path/to/ttstream-inherited --out target/inherited
python3 -m kitex_streaming_interop --profile /path/to/custom-profile.toml --suite metadata-wire --go-bin /path/to/custom-oracle --rust-bin /path/to/custom-streaming-fixture --out target/metadata-wire
python3 -m kitex_streaming_interop --profile /path/to/custom-profile.toml --suite error-multihop --go-bin /path/to/custom-oracle --rust-bin /path/to/custom-streaming-fixture --out target/business-multihop
bin/oracle --role payload-matrix --report target/payload-matrix.json
bin/oracle --role frame-golden --report target/frame-golden.json
```

`idl-matrix` compares generator/compile outcomes and diagnostics against the reviewed
fixture, including intentional upstream legacy failures. Changed input or unknown
cases are INCOMPLETE until reviewed. A compiler/network failure cannot substitute
for the expected semantic diagnostic. `--generation-only` never passes full IDL
acceptance. Scalar/container StreamX signatures compile in Go but the actual
TTStream payload encoder rejects them; `payload-matrix` verifies that distinction.

`raw-basic` and `typed-basic` exercise both network directions over TCP and UDS,
all three streaming modes and 0/1/8 messages. Typed adds the two unary modes and
all configured pools. They compare every response and successful terminal status.
The raw fixture must use checked `RawStruct` values with `RawSender`/`RawStream`
and report `raw_struct_validated: true` in Rust readiness/client results. Legacy
unchecked byte fixtures are rejected. Use `--go-api binary --mode client,server,bidi`
to exercise Kitex's actual BinaryThriftGenericV2 API against either Rust fixture.
Each invocation creates a client, so these tests do **not** establish pool reuse,
capacity, performance or long-lived resource behavior. Those remain separate gates.

`inherited-service` uses the generated `Child extends Parent` client and server.
One client calls all six inherited/own methods on the same address: ordinary unary
with throws, explicit unary, client streaming, server streaming and bidi. Responses
carry a different offset for each method, so exchanging handlers cannot pass by
returning identical echo data. The 40 TCP/UDS cases cover both directions, three
pools, 0/1/8 messages and shared handler state. The dedicated Rust fixture is
`tests/ttstream-codegen/src/bin/ttstream-inherited.rs`; its Go IDLs and generated
code are committed under `go/idl` and `go/kitex_gen`. Reproduce generation from
`go/` with the fixed generator: `kitex -module
github.com/cloudwego/volo/tests/interop/kitexstreaming -streamx idl/audit.thrift`.

`metadata-wire` requires a custom fixture implementing `metadata-wire-client`.
An actual Go TTHeader codec peer sends META before/after HEADER, empty and invalid
addresses, IPv6, derived IDC cases and unknown binary values. Assertions compare
each integer/string metadata key with its raw value bytes before JSON conversion;
whole-frame substring matches do not suffice. The fixture reports real initial
and final public contexts, local deadline and received DATA/Trailer. Outgoing
HEADER timeout keys 12/13/17 are forbidden. This is a direct TCP/UDS test of metadata
handling, with no Mesh process or claim about a production proxy deployment.

`error-multihop` requires the fixture's `--scenario errors --forward` role. All
eight Go/Rust A/B/C combinations run over TCP/UDS. The real downstream call returns
DATA followed by Biz code/message/extra; each forwarder preserves public metadata,
reports its observed terminal and releases child/handler resources. A recovery call
must also complete. Go DATA and a terminal error returned together are recorded as
such, never replaced with synthetic DATA. This suite does not claim transparent
forwarding of Go's private ApplicationException cause.

`wire-boundaries` uses Go's actual TTHeader encoder/decoder and generated Thrift
payload codec against both the Rust server and the Go baseline. It checks signed
IDs, unknown/duplicate IDs, RST followed by Trailer, delayed headers, per-mode
tolerance, routing, fragmented writes and interleaved streams. Inputs, sent/received
hex and decoded terminal exceptions are retained. Go's unknown-method timeout and
Rust's explicit UNKNOWN_METHOD response have separate expectations. A real Kitex
StreamX client separately verifies Rust maps to outer TypeId=12001 before its
deadline, Go times out with 12015, and a following valid call succeeds. Other error
families and lifecycle checks remain separate gates.

`idle-unary` connects once, waits five minutes before the first unary request,
then waits another five minutes before the second request on that same socket.
Independent Rust/Go TCP/UDS cases run concurrently. There is no reconnect path.
Both intervals, exact frames, replies and binary hashes are recorded. A smaller
`--idle-seconds` value is a codec smoke check and returns INCOMPLETE, never full
idle acceptance. Binary changes during the test fail the report.

Custom Rust endpoints may additionally implement `--rust-api binary|json|map`.
Use `typed-basic --direction rust-to-go --mode client,server,bidi` with this option;
the runner checks the reported API as well as every decoded response. This does
not substitute for generic server-side verification in Rust.

`--go-api binary --mode client,server,bidi` uses actual Kitex
`BinaryThriftGenericV2("Interop")` APIs. Go clients use the three generic streaming
entry points, and Go servers use the generic Bidi handler, which receives every
streaming mode. Messages contain ordinary struct bytes with exact decode
consumption, without unary message/Args/Result envelopes. Both endpoint directions
are supported; `--rust-api binary|json|map --direction rust-to-go` additionally
tests a Rust conversion API against the Go generic server. Reports check the
declared API, profile, every response and terminal status. This fixture does not
claim Rust generic-server routing or arbitrary IDL schemas are covered.

`metainfo` runs actual typed Bidi A→B→C calls for every Go/Rust role combination
over TCP/UDS. Go intermediate nodes run explicit SingleHop, explicit Legacy and
their profile's unmodified default; Rust always uses single-hop. Each case checks
persistent propagation, transient expiry, resetting and overwriting at B, empty
keys/values, same-key priority, and
receiver-local deadline absence. Rust records the real incoming HEADER, including
absence of timeout/deadline fields. Go's public ServerStream has no incoming raw
HEADER accessor, so its raw header fields are null, not fabricated empty maps.

The fixture server flags are `--scenario metainfo --node B|C --forward <address>`
(forward is optional). `metadata-client --input seed.json` sends one seed;
`metadata-batch-client` accepts an array of up to 32 seeds, uses one configured
mux client, and returns ordered results after every stream reaches its terminal
status. `--metainfo-concurrency 16` enables this batch path and verifies distinct
identities and values for every stream. This is context-isolation evidence; actual
physical connection counts, cascading cancellation and resource convergence still
have their own gates. A mode override only affects the test Go process's provider;
it is not a production configuration change or a Rust legacy-mode implementation.
Additional Go codec inputs carry `RPC_TRANSIT_UPSTREAM_*` values, alongside current
transient values and an unknown metadata key. They verify stale upstream handling,
current-value priority and preservation of unknown incoming Rust metadata. These
raw cases are individual streams, separate from the concurrent typed batches.

`cascade` runs actual A→B→C Bidi calls with explicit cancellation and a three-second
client deadline. C acknowledges the live call before A cancels; B keeps both
directions open and waits on its real child call. Fixtures use the framework's
handler context, without manually linking cancellation or calling a child's
cancel method. The runner requires local/remote codes 12007 or 12015, B's observed
path `[A]`, C's observed path `[A,B]`, exactly one handler completion, child release,
zero remaining fixture handlers and a successful recovery call using the same
configured client. Go reports the path rendered by its actual Exception because
the path field is private. This does not replace connection/driver resource
accounting, receive-timeout KeepOpen, server-configured timeout or soak checks.
Use `--topology ggg` for the reference baseline, or all eight Go/Rust combinations
with `--topology all`. Server flags are `--scenario cascade --node B|C --forward
<address>`; clients use `--role cascade-client --mode cancel|deadline --payload
<case-id>`. Recovery uses `<case-id>-probe` to keep lifecycle events unambiguous.

`error-status` checks a DATA response followed by a business status or an
ApplicationException, custom Header/Trailer values and a subsequent successful
call. Four client/server language combinations plus Go wire clients run over
TCP/UDS (24 cases). `--reference-only` runs the eight Go reference cases. Go's
typed Exception does not expose the application cause: its inner code is reported
as unavailable, while a separate wire case decodes the actual payload code 6.
Go's handler wrapper prefixes an application message with `biz error: `; the
runner checks this exact server-side difference instead of stripping it.

Kitex can return a fully decoded response **and** a business error in the same
`Recv` if its read loop has already processed the Trailer. The fixture records
both real return values and labels `data_and_error_same_recv`; it does not invent
a successful first receive. This was also reproduced against Go servers using
`error-client --mode biz --recv-delay 50ms`, an intentional slow consumer. Rust
must preserve separate DATA and terminal observations, and wire cases always
require HEADER/DATA/TRAILER order. No transport sleep or disabled batching is
used to hide this Go API behavior. Timeout policies remain separate gates.

`response-boundaries` uses a Go-codec raw responder and actual Go/Rust typed clients
over TCP/UDS (52 cases, or 26 Go cases with `--reference-only`). The responder waits
for the client's HEADER/DATA/Trailer before sending anything, proving open does
not wait for a response Header. Cases include duplicate Header, malformed
Trailer/RST, missing RST payload, cancellation code, invalid/zero business status,
invalid business extra, application-over-business precedence, and tolerated trailing
bytes inside DATA/Exception payloads. The runner checks actual API results and
retains exact request/response frame bytes. Invalid numeric `biz-status` is ignored
by Go; malformed extra with a valid nonzero status yields 12003. EOF or errors
before the receive stage cannot pass. These are single-stream cases; sibling stream
isolation, connection reuse after faults and timeout policies need their own tests.
The endpoint roles are `raw-response-server --input scenario.json` and
`response-client`; the latter reports errors without asserting expected codes.
For Rust generic clients, add `--rust-api binary|json|map`; each invocation runs
the 26 Rust cases and verifies the reported conversion API. DATA suffix cases
also require the exact payload observed before conversion, including its suffix.
This does not replace the Go reference or typed-client runs.

For Rust generic servers, use `typed-basic --direction go-to-rust --go-api binary
--rust-api binary|json|map --mode client,server,bidi`. The runner starts the actual
`--scenario generic` server, checks `ready.rust_api`, and compares all messages and
terminals across all configured pools. With three pools and TCP/UDS this gives
54 cases per conversion API.

`service-routing` checks real public registration and the eight unique/fallback/
strict/combine/unknown/dynamic/binary-v2/mixed configurations (186 cases). Each
wire case validates the selected handler and actual service/method context, then
makes a successful call on the same socket. Known-service lookup does not fall
through to a different service merely because a method is absent. Unknown-method
Go silence and Rust ApplicationException(1) are separate expectations. A TTStream
call to a unary descriptor must not be misrouted to another service's streaming
method; Go's generated unary adapter panic and Rust's explicit rejection are
recorded separately. Registration records Go panic versus Run error and Rust
validation errors without imitating Go panics.

`timeout-policy` uses the separate `ttstream-lifecycle` Rust endpoint. It covers
16 Go/Rust combinations of actual per-Recv KeepOpen/Cancel and four Rust server
local deadline cases. Live ACK, release after KeepOpen, successful EOF, recovery,
12014/12015 and actual cancellation paths are required. `drain-disconnect` adds
14 cases: kill an acknowledged peer, close a connection while its listener lives,
and drain while sending another HEADER and releasing the original stream on the
same socket. Explicit stdout events/stdin barriers control ordering. It tests
Connection drain; full generated Server.run signal admission is a separate gate.

`server-shutdown` covers that production path with the endpoint's `mixed-server`
role. A Go codec probe retains one unary socket and one active streaming socket
across SIGTERM. After the actual shutdown hook runs, unary must still succeed,
new streams must receive RST(6), and the original stream must finish normally
before the server exits successfully. At most 64 post-hook probes and 500ms allow
the hook-to-admission scheduling window without a fixed sleep. Both sockets must
have been dialed exactly once. This does not establish forced-deadline or flush
failure behavior.

`budget-limits` uses an application-gated consumer and the actual Go typed mux
client. The same 128 messages of 1KiB and a larger 2,048-message/16KiB sample
(32MiB of application data) must succeed with Go's unbounded queue and Rust's
default budgets. Separately tightening Rust's per-stream message/byte,
connection-byte and shared-byte budgets must yield TypeId=6 and
`errors.Is(err, kerrors.ErrStreamingCanceled)`, while a later stream progresses.
The runner waits for that later probe before releasing the consumer. Cancellation
is tested while the original application still does not consume queued messages.

Raw Go-codec cases independently require RST payload code 6 and same-socket
recovery. They fill two stream queues on a single socket, confirm occupancy using
actual Rust resource snapshots, then send RST and another HEADER. They also exceed
an 8KiB aggregate budget using two individually smaller streams, on one connection
and on two separate connections, and require the non-offending stream to preserve
all four messages. Frame bytes, counters, active streams/gates and configuration
are retained. These 28 cases use a 500ms later-control bound and a two-second
resource convergence bound. Only the explicit full-queue tests set a 20ms read
pause; the default remains unchanged. This is a receive-admission gate, not a
send-budget, production-RSS, or complete long-duration performance claim.
Overflow terminal snapshots must include a positive control-queue delay measured
through flush or discard; an unmeasured default zero cannot satisfy that check.

`shutdown-boundaries` additionally checks accepted sockets before the first byte
or with only a four-byte frame prefix, an active stream that never finishes before
the configured 100ms drain deadline, and peer EOF immediately after the signal.
It requires normal server exit, one terminal per active stream and the actual
deadline RST(6) with the `ServerDraining` message and local code 6. Accepted/ACK
and shutdown-hook events synchronize each case;
incomplete first-frame sockets must close within 500ms without a protocol reply.
Driver tests separately inject a blocked or failed `AsyncWrite::flush`.

Current Kitex/netpoll clients wrap netpoll EOF as 12004 while Rust reports 12013.
This is a recorded local error-classification difference, verified against Go
servers as well as Rust. The server-side 12013 rule must not be assumed for Go
clients; see `evidence/go-client-eof/` for a separately executed codec reproducer.
Neither a missing terminal nor a silent retry is accepted as successful closure.

The server emits JSON readiness only after binding/starting. The runner uses
dynamic ports, process groups, bounded timeouts and deterministic cleanup, recording
stdout/stderr and the exact failed invocation. It never uses a fixed startup sleep.
Custom profiles provide a profile.toml and a binary reporting the same profile
name; mismatched binaries fail instead of silently testing upstream Kitex.
An optional `[environment.set]` table explicitly overrides environment variables
for both endpoints (for example an ephemeral debug port). Reports include
only these overrides, never the full inherited environment. UDS sockets use an
owned short temporary directory so deeply nested artifact paths remain usable.

Unsupported suites return INCOMPLETE/nonzero. Real service-mesh integration and
ShmIPC runtime validation are outside the current validation scope; neither is
covered by these TCP/UDS results.

Golden regeneration writes a proposal under `--out`, never overwrites the checked-in
oracle automatically. Maps/sets use semantic comparison where iteration order is
not canonical. Floating-point values use IEEE bits, including NaN payloads.

Runner lifecycle regression tests:

```sh
PYTHONPATH=python python3 -m unittest discover -s tests -v
```

## Environment validation and process isolation

Only `--protocol ttstream` and `--env-profile direct` are supported. Unknown
profiles and unsupported protocols fail before suite dispatch; real Mesh scenarios
remain deferred. Profile `environment.required` is a list of unique nonempty
variable names, checked after inherited environment and `environment.set` merge.
Diagnostics include names only. Pass secrets through the parent environment and
list their names in `required`; `set` is for nonsecret fixture configuration that
existing reports intentionally record. The summary records only variable names
and the validated contract, never inherited values.

The supported isolation contract is `process_isolation = "scenario-group"`.
Typed/wire suites intentionally reuse a server within a direction/transport
group; client cases use separate processes, while concurrency and multihop cases
retain the shared state required by their scenarios. Every group owns and reaps
its children through the shared process helper. `fresh_process_per_case=true` or
any stronger isolation value is rejected, not silently promised.
