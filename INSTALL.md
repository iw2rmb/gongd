# Install


## Homebrew

```bash
brew tap iw2rmb/gongd https://github.com/iw2rmb/gongd
brew install gongd
brew services start gongd
```


## From Sources

```bash
cargo install --path .
```


### Service install

Template units are provided in `deploy/`:

- `deploy/gongd.service` for `systemd --user`
- `deploy/local.gongd.plist` for `launchd`

> They invoke `gongd` directly, so the service environment must have `gongd` on `PATH`. If you install with `cargo install --path .`, ensure `~/.cargo/bin` is visible to `systemd --user` or `launchd`.
>
> If you want fixed startup repos from the service definition, append them to `ExecStart` or `ProgramArguments`. They seed `~/.gong/config.json` only when the file is missing or empty.


#### macOS launchd

```bash
cp deploy/local.gongd.plist ~/Library/LaunchAgents/local.gongd.plist
launchctl unload ~/Library/LaunchAgents/local.gongd.plist 2>/dev/null || true
launchctl load ~/Library/LaunchAgents/local.gongd.plist
launchctl start local.gongd
```


#### Linux systemd

```bash
mkdir -p ~/.config/systemd/user
cp deploy/gongd.service ~/.config/systemd/user/gongd.service
systemctl --user daemon-reload
systemctl --user enable --now gongd
```
