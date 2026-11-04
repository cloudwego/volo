package oracle

import (
	"encoding/json"
	"os"
	"testing"
)

func TestRoutingRegistrationObservations(t *testing.T) {
	path := t.TempDir() + "/registration.json"
	if err := routingRegistration(Profile{Name: "upstream", KitexModule: "github.com/cloudwego/kitex v0.16.4"}, path); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	t.Log(string(data))
	var report struct {
		OK    bool `json:"ok"`
		Cases []struct {
			Outcome string `json:"outcome"`
			Stage   string `json:"stage"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(data, &report); err != nil {
		t.Fatal(err)
	}
	if !report.OK || len(report.Cases) != 9 {
		t.Fatalf("unexpected registration outcomes: %s", data)
	}
	for i, item := range report.Cases {
		if i < 3 && (item.Stage != "register" || item.Outcome != "panic") {
			t.Fatalf("expected actual public registration panic: %+v", item)
		}
		if i >= 3 && (item.Stage != "run" || item.Outcome != "error") {
			t.Fatalf("expected Run validation error: %+v", item)
		}
	}
}
