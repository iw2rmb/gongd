package gongd

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestAddWatch(t *testing.T) {
	socket := testSocketPath(t, "add")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer listener.Close()

	requests := make(chan map[string]any, 1)
	go serveOneControlConn(t, listener, requests, ControlResponse{
		OK:      true,
		Message: stringPtr("watch added"),
	})

	client := NewClient()
	client.ControlSocket = socket

	response, err := client.AddWatch(context.Background(), "/tmp/repo")
	if err != nil {
		t.Fatalf("AddWatch: %v", err)
	}
	if !response.OK {
		t.Fatalf("response not ok: %#v", response)
	}

	request := <-requests
	if request["op"] != "add_watch" {
		t.Fatalf("unexpected op: %#v", request["op"])
	}
	if request["repo"] != "/tmp/repo" {
		t.Fatalf("unexpected repo: %#v", request["repo"])
	}
}

func TestVersion(t *testing.T) {
	if Version != "0.1.0" {
		t.Fatalf("unexpected version: %q", Version)
	}
}

func TestListWatches(t *testing.T) {
	socket := testSocketPath(t, "list")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer listener.Close()

	go serveOneControlConn(t, listener, nil, ControlResponse{
		OK:    true,
		Repos: []string{"/tmp/a", "/tmp/b"},
	})

	client := NewClient()
	client.ControlSocket = socket

	repos, err := client.ListWatches(context.Background())
	if err != nil {
		t.Fatalf("ListWatches: %v", err)
	}
	if len(repos) != 2 || repos[0] != "/tmp/a" || repos[1] != "/tmp/b" {
		t.Fatalf("unexpected repos: %#v", repos)
	}
}

func TestRemoveWatchReturnsDaemonError(t *testing.T) {
	socket := testSocketPath(t, "remove")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer listener.Close()

	go serveOneControlConn(t, listener, nil, ControlResponse{
		OK:    false,
		Error: stringPtr("watch not found"),
	})

	client := NewClient()
	client.ControlSocket = socket

	_, err = client.RemoveWatch(context.Background(), "/tmp/repo")
	if err == nil {
		t.Fatal("expected error")
	}
	var daemonErr *DaemonError
	if !strings.Contains(err.Error(), "watch not found") || !errors.As(err, &daemonErr) {
		t.Fatalf("unexpected error: %T %v", err, err)
	}
}

func TestSubscribe(t *testing.T) {
	socket := testSocketPath(t, "events")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer listener.Close()

	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		_, _ = conn.Write([]byte("{\"repo\":\"/tmp/repo\",\"type\":\"file_modified\",\"path\":\"main.go\",\"git_path\":null,\"ts_unix_ms\":1}\n"))
		_, _ = conn.Write([]byte("{\"repo\":\"/tmp/repo\",\"type\":\"repo_head_changed\",\"path\":null,\"git_path\":\"HEAD\",\"ts_unix_ms\":2}\n"))
	}()

	client := NewClient()
	client.EventSocket = socket

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	events, errs := client.Subscribe(ctx)
	first := <-events
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
	if err := <-errs; !errors.Is(err, io.EOF) {
		t.Fatalf("unexpected subscription error: %v", err)
	}
}

func serveOneControlConn(t *testing.T, listener net.Listener, requests chan<- map[string]any, response ControlResponse) {
	t.Helper()

	conn, err := listener.Accept()
	if err != nil {
		t.Errorf("accept: %v", err)
		return
	}
	defer conn.Close()

	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		t.Errorf("read request: %v", err)
		return
	}
	if requests != nil {
		var request map[string]any
		if err := json.Unmarshal(line, &request); err != nil {
			t.Errorf("decode request: %v", err)
			return
		}
		requests <- request
	}

	raw, err := json.Marshal(response)
	if err != nil {
		t.Errorf("encode response: %v", err)
		return
	}
	raw = append(raw, '\n')
	if _, err := conn.Write(raw); err != nil {
		t.Errorf("write response: %v", err)
	}
}

func testSocketPath(t *testing.T, name string) string {
	t.Helper()

	return filepath.Join("/tmp", "go-gongd-"+name+"-"+time.Now().Format("150405.000000000")+".sock")
}

func stringPtr(v string) *string {
	return &v
}
