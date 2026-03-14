package gongd

import (
	"context"
	"net"
)

const (
	Version              = "v0.1.0"
	DefaultEventSocket   = "/tmp/gongd.sock"
	DefaultControlSocket = "/tmp/gongd.ctl.sock"
)

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
