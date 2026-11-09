// Package oracle owns the shared Go scenarios. Custom Kitex profiles provide
// only framework initialization options; they do not duplicate these scenarios.
package oracle

import (
	"context"

	"github.com/cloudwego/kitex/client"
	"github.com/cloudwego/kitex/pkg/serviceinfo"
	"github.com/cloudwego/kitex/server"
)

type Profile struct {
	Name          string
	KitexModule   string
	DefaultPool   string
	ClientOptions func(*serviceinfo.ServiceInfo, string) []client.Option
	ServerOptions func(*serviceinfo.ServiceInfo) []server.Option
	// SnapshotMetadata observes a profile's SDK state without embedding SDK-specific
	// metadata names in the shared scenarios. It is only used by the metadata probe.
	SnapshotMetadata func(context.Context) map[string]string
	RequestMetadata  func(context.Context) map[string]string
}
