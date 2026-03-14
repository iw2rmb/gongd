# go-gongd

`go-gongd` is a small Go SDK for `gongd`.

It wraps the two daemon sockets:

- `/tmp/gongd.sock` for the event stream
- `/tmp/gongd.ctl.sock` for control requests

## Install

From this repository:

```bash
cd sdk/go-gongd
go test ./...
```

## Usage

```go
package main

import (
	"context"
	"fmt"

	gongd "go-gongd"
)

func main() {
	client := gongd.NewClient()

	if _, err := client.AddWatch(context.Background(), "/absolute/path/to/repo"); err != nil {
		panic(err)
	}

	events, errs := client.Subscribe(context.Background())
	for {
		select {
		case event := <-events:
			fmt.Println(event.Type, event.Repo, event.Path, event.GitPath)
		case err := <-errs:
			panic(err)
		}
	}
}
```
