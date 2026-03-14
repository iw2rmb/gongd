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
	"time"
)

const reconnectDelay = 100 * time.Millisecond

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

		for {
			retry, err := c.readEventStream(ctx, conn, events)
			_ = conn.Close()
			if err != nil {
				sendErr(errs, err)
				return
			}
			if !retry {
				return
			}

			conn, err = c.redialEventStream(ctx)
			if err != nil {
				if ctx.Err() == nil {
					sendErr(errs, err)
				}
				return
			}
			if conn == nil {
				return
			}
		}
	}()

	return events, errs
}

func (c *Client) readEventStream(ctx context.Context, conn net.Conn, events chan<- Event) (bool, error) {
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
				return false, fmt.Errorf("decode event: %w", decodeErr)
			}

			select {
			case events <- event:
			case <-ctx.Done():
				return false, nil
			}
		}

		if err == nil {
			continue
		}
		if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
			return false, nil
		}
		if errors.Is(err, io.EOF) {
			return true, nil
		}
		return true, nil
	}
}

func (c *Client) redialEventStream(ctx context.Context) (net.Conn, error) {
	for {
		conn, err := c.dial(ctx, c.EventSocket)
		if err == nil {
			return conn, nil
		}
		if ctx.Err() != nil {
			return nil, nil
		}

		timer := time.NewTimer(reconnectDelay)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, nil
		case <-timer.C:
		}
	}
}
