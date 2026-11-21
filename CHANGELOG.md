# Changelog

## 0.1.1

- Stop embedded zuse programs when a shared operation reports a structured
  failure, including partial PK teardown and expired verification. Preserve
  completed steps and redacted failure details, and skip later mutations.
- Exclude root Git administration files from dependency publication scope
  while retaining complete source and registry archive comparisons.
- Build Windows releases with the initialized native compiler environment,
  and support release retries against an unchanged, verified tag.
- Keep native C/C++ archives linkable in AUR builds by disabling makepkg's
  injected LTO flags; Cargo's configured Rust release optimization remains.

## 0.1.0

The first Rust implementation uses one operation dispatcher for the zgpui desktop,
zclap CLI, zmcpio MCP service, local HTTP API and embedded zuse programs.

- Bilibili account login and switching, room metadata, RTMP/SRT selection,
  cover preview and 16:9 cropping, recent titles and areas, audit polling,
  delay control and platform verification handoff.
- OBS WebSocket configuration, output observation and live-state linking;
  PK signalling, invitation events and native libdatachannel/FFmpeg media;
  the attributed OBS capture plugin is distributed as source.
- Scene conversion, typed operation schemas, bounded zuse execution,
  operating-system credential storage, font/background preferences, tray
  integration and per-profile single-instance activation.
- `--endpoint` lets CLI commands share the state of an existing local GUI or
  service. The foreground `rtc preview-local` command keeps a local preview
  alive until cancellation.
- Default-branch Project Z development dependencies, a committed dependency
  receipt, verified registry staging, formal release builds and separate
  AUR application/plugin packages.

The dependency refresh uses zconv 0.4.1, zcall 0.4.1, zmcpio 0.5.0,
zuse-computer 0.4.0 and zgpui 0.1.6. Headless tests passed after this update.
Earlier native validation also covered a complete Chinese-language window,
four background pixel samples, local RTC/encoding and isolated OBS 33 module
loading. Snapshot identifiers and the scope of each result are in
[VALIDATION.md](docs/VALIDATION.md).

Authenticated platform broadcasts, two-party PK media and macOS/Windows native
runtime acceptance are still separate gates. See
[COMPATIBILITY.md](docs/COMPATIBILITY.md) for the audited StartLive scope.
