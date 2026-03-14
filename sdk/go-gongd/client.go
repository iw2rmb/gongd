package gongd

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
)

const (
	DefaultEventSocket   = "/tmp/gongd.sock"
	DefaultControlSocket = "/tmp/gongd.ctl.sock"
)

type EventType string

const (
	EventFileCreated           EventType = "file_created"
	EventFileModified          EventType = "file_modified"
	EventFileDeleted           EventType = "file_deleted"
	EventFileRenamed           EventType = "file_renamed"
	EventDirCreated            EventType = "dir_created"
	EventDirDeleted            EventType = "dir_deleted"
	EventDirRenamed            EventType = "dir_renamed"
	EventRepoHeadChanged       EventType = "repo_head_changed"
	EventRepoIndexChanged      EventType = "repo_index_changed"
	EventRepoRefsChanged       EventType = "repo_refs_changed"
	EventRepoPackedRefsChanged EventType = "repo_packed_refs_changed"
	EventRepoChanged           EventType = "repo_changed"
)

type Event struct {
	Repo     string    `json:"repo"`
	Type     EventType `json:"type"`
	Path     *string   `json:"path"`
	GitPath  *string   `json:"git_path"`
	TSUnixMS uint64    `json:"ts_unix_ms"`
}

type ControlResponse struct {
	OK      bool     `json:"ok"`
	Message *string  `json:"message,omitempty"`
	Error   *string  `json:"error,omitempty"`
	Repos   []string `json:"repos,omitempty"`
}

type DaemonError struct {
	Message string
}

func (e *DaemonError) Error() string {
	return e.Message
}

type Client struct {
	EventSocket   string
	ControlSocket string
	DialContext   func(ctx context.Context, network, address string) (net.Conn, error)
}

func NewClient() *Client {
	dialer := &net.Dialer{}
	return &Client{
		EventSocket:   DefaultEventSocket,
		ControlSocket: DefaultControlSocket,
		DialContext:   dialer.DialContext,
	}
}

func (c *Client) Subscribe(ctx context.Context) (<-chan Event, <-chan error) {
	events := make(chan Event)
	errs := make(chan error, 1)

	go func() {
		defer close(events)
		defer close(errs)

		conn, err := c.dial(ctx, c.EventSocket)
		if err != nil {
			sendErr(errs, err)
			return
		}
		defer conn.Close()

		done := make(chan struct{})
		go func() {
			select {
			case <-ctx.Done():
				_ = conn.Close()
			case <-done:
			}
		}()
		defer close(done)

		reader := bufio.NewReader(conn)
		for {
			line, err := reader.ReadBytes('\n')
			line = bytes.TrimSpace(line)
			if len(line) > 0 {
				var event Event
				if decodeErr := json.Unmarshal(line, &event); decodeErr != nil {
					sendErr(errs, fmt.Errorf("decode event: %w", decodeErr))
					return
				}

				select {
				case events <- event:
				case <-ctx.Done():
					return
				}
			}

			if err != nil {
				if errors.Is(err, io.EOF) && ctx.Err() == nil {
					sendErr(errs, io.EOF)
				} else if ctx.Err() == nil && !errors.Is(err, net.ErrClosed) {
					sendErr(errs, err)
				}
				return
			}
		}
	}()

	return events, errs
}

func (c *Client) AddWatch(ctx context.Context, repo string) (*ControlResponse, error) {
	return c.doControl(ctx, addWatchRequest{
		Op:   "add_watch",
		Repo: repo,
	})
}

func (c *Client) RemoveWatch(ctx context.Context, repo string) (*ControlResponse, error) {
	return c.doControl(ctx, removeWatchRequest{
		Op:   "remove_watch",
		Repo: repo,
	})
}

func (c *Client) ListWatches(ctx context.Context) ([]string, error) {
	response, err := c.doControl(ctx, listWatchesRequest{Op: "list_watches"})
	if err != nil {
		return nil, err
	}
	return response.Repos, nil
}

type addWatchRequest struct {
	Op   string `json:"op"`
	Repo string `json:"repo"`
}

type removeWatchRequest struct {
	Op   string `json:"op"`
	Repo string `json:"repo"`
}

type listWatchesRequest struct {
	Op string `json:"op"`
}

func (c *Client) doControl(ctx context.Context, request any) (*ControlResponse, error) {
	conn, err := c.dial(ctx, c.ControlSocket)
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	done := make(chan struct{})
	go func() {
		select {
		case <-ctx.Done():
			_ = conn.Close()
		case <-done:
		}
	}()
	defer close(done)

	raw, err := json.Marshal(request)
	if err != nil {
		return nil, fmt.Errorf("encode request: %w", err)
	}
	raw = append(raw, '\n')
	if _, err := conn.Write(raw); err != nil {
		return nil, err
	}

	line, err := bufio.NewReader(conn).ReadBytes('\n')
	if err != nil {
		return nil, err
	}

	var response ControlResponse
	if err := json.Unmarshal(bytes.TrimSpace(line), &response); err != nil {
		return nil, fmt.Errorf("decode response: %w", err)
	}
	if !response.OK {
		if response.Error != nil {
			return &response, &DaemonError{Message: *response.Error}
		}
		return &response, &DaemonError{Message: "gongd returned an unsuccessful response"}
	}
	return &response, nil
}

func (c *Client) dial(ctx context.Context, socket string) (net.Conn, error) {
	if c.DialContext != nil {
		return c.DialContext(ctx, "unix", socket)
	}
	dialer := &net.Dialer{}
	return dialer.DialContext(ctx, "unix", socket)
}

func sendErr(errs chan<- error, err error) {
	select {
	case errs <- err:
	default:
	}
}
