package gongd

import (
	"context"
	"net"
	"os"
	"testing"
	"time"
)

func TestSubscribeReconnectsAfterServiceReload(t *testing.T) {
	socket := testSocketPath(t, "events")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer func() {
		_ = listener.Close()
		_ = os.Remove(socket)
	}()

	ready := make(chan struct{}, 1)

	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		_, _ = conn.Write([]byte("{\"repo\":\"/tmp/repo\",\"type\":\"file_modified\",\"path\":\"main.go\",\"git_path\":null,\"ts_unix_ms\":1}\n"))
		_ = conn.Close()
		_ = listener.Close()
		_ = os.Remove(socket)

		reloaded, err := net.Listen("unix", socket)
		if err != nil {
			t.Errorf("reload listen: %v", err)
			return
		}
		listener = reloaded
		ready <- struct{}{}

		conn, err = listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		_, _ = conn.Write([]byte("{\"repo\":\"/tmp/repo\",\"type\":\"repo_head_changed\",\"path\":null,\"git_path\":\"HEAD\",\"ts_unix_ms\":2}\n"))
	}()

	client := NewClient()
	client.EventSocket = socket

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	events, errs := client.Subscribe(ctx)
	first := <-events
	select {
	case <-ready:
	case <-ctx.Done():
		t.Fatal("timed out waiting for socket reload")
	}
	second := <-events

	if first.Type != EventFileModified {
		t.Fatalf("unexpected first event: %#v", first)
	}
	if first.Path == nil || *first.Path != "main.go" {
		t.Fatalf("unexpected first path: %#v", first.Path)
	}
	if second.Type != EventRepoHeadChanged {
		t.Fatalf("unexpected second event: %#v", second)
	}
	if second.GitPath == nil || *second.GitPath != "HEAD" {
		t.Fatalf("unexpected second git path: %#v", second.GitPath)
	}

	cancel()
	select {
	case err := <-errs:
		if err != nil {
			t.Fatalf("unexpected subscription error: %v", err)
		}
	case <-time.After(500 * time.Millisecond):
	}
}
