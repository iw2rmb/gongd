# go-gongd

`go-gongd` is a small Go SDK for `gongd`.

Current release: `v0.1.4`.

It wraps the two daemon sockets:

- `/tmp/gongd.sock` for the event stream
- `/tmp/gongd.ctl.sock` for control requests

Event subscriptions reconnect automatically after daemon restarts or socket reloads.

## Install

From this repository:

```bash
cd sdk/go-gongd
go test ./...
```

From another Go module:

```bash
go get github.com/iw2rmb/gongd/sdk/go-gongd@v0.1.4
```

## Usage

```go
package main

import (
	"context"
	"fmt"

	gongd "github.com/iw2rmb/gongd/sdk/go-gongd"
)

func main() {
	client := gongd.NewClient()

	if _, err := client.AddWatch(context.Background(), "/absolute/path/to/folder"); err != nil {
		panic(err)
	}

	events, errs := client.Subscribe(context.Background())
	for {
		select {
		case event := <-events:
			fmt.Println(event.Type, event.Folder, event.Path, event.GitPath)
		case err := <-errs:
			panic(err)
		}
	}
}
```
