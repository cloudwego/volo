// Package oracle owns the shared Go scenarios. Custom Kitex profiles provide
// only framework initialization options; they do not duplicate these scenarios.
package oracle

import (
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
}
