package gongd

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
)

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
