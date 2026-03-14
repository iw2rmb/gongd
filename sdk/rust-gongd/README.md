# rust-gongd

`gongd-sdk` is a small Rust SDK for `gongd`.

It wraps the two daemon sockets:

- `/tmp/gongd.sock` for the event stream
- `/tmp/gongd.ctl.sock` for control requests

## Install

From this repository:

```bash
cd sdk/rust-gongd
cargo test
```

## Usage

```rust
use gongd_sdk::Client;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();

    client.add_watch("/absolute/path/to/repo")?;

    let mut stream = client.subscribe()?;
    while let Some(event) = stream.next_event()? {
        println!("{:?} {} {:?} {:?}", event.event_type, event.repo, event.path, event.git_path);
    }

    Ok(())
}
```
