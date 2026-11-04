package main

import "github.com/cloudwego/volo/tests/interop/kitexstreaming/oracle"

func main() {
	oracle.Main(oracle.Profile{Name: "upstream", KitexModule: "github.com/cloudwego/kitex@v0.16.4", DefaultPool: "mux"})
}
