# Go 客户端断连错误码：实际复现与源码链路

结论：当前固定 Kitex v0.16.4 / netpoll v0.7.2 的客户端收到纯连接 EOF，完全可以返回12004，而非12013。这个结果由具体错误包装决定，不需要假设 OnInactive 竞态；Rust客户端的12013契约可保持，兼容性矩阵记录本地错误分类差异。

## 已执行最简复现

`eof_probe.go` 实现bufiox.Reader，其首个Next返回 `netpoll.Exception(netpoll.ErrEOF,"peer close")`；调用实际 `ttstream.DecodeFrame`，另以普通io.EOF作对照。不修改Kitex源码、不伪造TypeId，也不依赖真实断线调度。

```sh
# cwd=$VOLO_REPO/tests/interop/kitex-streaming/go
GOTOOLCHAIN=local GOPROXY=off GOMODCACHE=/tmp/kitex-ttstream-modcache GOCACHE=/tmp/ttstream-integration-lpn9t2xb/go-cache GOPATH=/tmp/kitex-ttstream-gopath go run /tmp/ttstream-lifecycle-go/eof_probe.go
```

实际结果见eof-probe.json：netpoll EOF的`errors.Is(err,io.EOF)`为false，输出TypeId12004和完整`[ttstream error, code=12004] EOF peer close`；普通io.EOF原样返回，没有TypeId。这与root先收到live ACK再kill服务端的实际网络观测一致。网络kill结果由root独立报告，未将此确定性解码复现当作第二次网络测试。

## 原因与角色区别

- netpoll `connection_reactor.go:31` 在peer close触发 `Exception(ErrEOF,"peer close")`。
- `connection_errors.go:36,78-91`：ErrEOF是syscall.Errno(0x106)；Is支持自身及ErrConnClosed，没有将其映射为io.EOF。注释“Same as io.EOF”不等于errors.Is实现相同。
- Kitex `frame.go:133-141` 的DecodeFrame只对io.EOF直接返回，其余读取错误构建errIllegalFrame，码12004。
- `transport_client.go:70-84,123-129,204-214`：loopRead的错误由defer Close(err)，releaseResources直接s.close(err,...)，未重分类为12013。isIgnoreError只决定日志，不改错误。
- 对应12013包装在 **server** `transport_server.go:111-119` releaseResources，用errConnectionClosedCancel包装再关闭流；不能把该服务端行为外推到客户端。
- Kitex自带 `transport_test.go:294-304` 本身断言client receive可能为errIllegalFrame或errTransport，而服务端接收在314行断言errConnectionClosedCancel。并发写失败可能竞争成为关闭原因（12006）；当前root纯接收断线场景实测12004，不能凭该可能性宣称其他码已实际覆盖。

无需修改生产Rust或去伪造Go12013。fixture对Go断连接受实际观测码的修正由root维护；本agent未覆盖root已调整的lifecycle.go。
