# rusty-sound

A small command-line SoundCloud player for macOS, with a menu bar control.

> **Unofficial.** This project is not affiliated with, endorsed by, or supported by SoundCloud. It uses SoundCloud's undocumented web API (`api-v2`), which is not a public API and whose use is restricted by SoundCloud's terms of use. It is for personal experimentation only. Endpoints can change or stop working at any time.

## What it does

- Signs in through a Chrome window and keeps the session on your machine.
- Plays your liked tracks, with next, previous, pause, and reset to likes.
- Radio mode queues tracks related to the one playing.
- Runs as an on-demand background service that stops itself when idle.
- Every command has a `--json` mode for scripts and AI agents.
- A menu bar app (`notch/`) shows the current track and controls.

## Requirements

- macOS, Rust (stable), Swift command line tools (for the menu bar app)
- Google Chrome (for `login`)
- A SoundCloud account

## Usage

```sh
cargo build --release
cp target/release/rusty-sound ~/.local/bin/

rusty-sound login        # sign in with Chrome; saves the session
rusty-sound install      # register the launchd agent (does not start it)
rusty-sound start
rusty-sound play         # queue your likes and play
rusty-sound now
rusty-sound stop
```

Run `rusty-sound` with no arguments to see all commands.

## Files on disk

- `~/.config/rusty-sound/session.json`: session token (mode 600). Delete it to log out.
- `~/.config/rusty-sound/browser-profile/`: Chrome profile used for login.
- `~/Library/Logs/rusty-sound.log`: service log.

Tokens are secrets. Do not commit or share them.

## Layout

- `src/`: Rust CLI, background service, playback
- `notch/`: Swift menu bar app (`notch/build.sh`)

## Disclaimer

This software is provided as is, without warranty. You are responsible for how you use it and for complying with SoundCloud's terms.
