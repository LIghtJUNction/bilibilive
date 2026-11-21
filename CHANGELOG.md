# Changelog

## 0.1.3

- Add release packaging for Windows `Setup.exe`, macOS `Setup.pkg` and `.app`,
  complete Velopack portable archives, and separate platform/architecture
  update feeds. A complete portable installation retains its update helper
  and metadata; a bare executable or ordinary archive does not gain that
  managed-update identity.
- Add the desktop **Updates and components** page and shared
  `updates.check/status/download/apply` operations. Startup checks follow the
  saved preference and `--noupdate`; demo and read-only startup skip them.
  Download and apply each require an explicit action and a verified staged
  identity. Cargo and AUR installations retain their package-manager update
  path, and newer development versions are not offered a downgrade.
- Persist validated update receipts so separate CLI calls can check, download
  synchronously, and apply. Background downloads require a persistent host.
  Before applying, retire local observation and media sessions, then exit
  normally after the successful reply is flushed. OBS output and the live
  room retain their current state. A lost apply reply can be replayed with
  matching parameters within the helper's wait period without launching a
  second helper.
- Require `updates.apply` to be the last expanded step of a zuse program.
  Propagate terminal success and partial shutdown failures through the script
  receipt so GUI, HTTP and MCP hosts make the same exit decision. Avoid
  blocking that exit on a contended diagnostic log.
- Add verified local OBS plugin status and explicit installation from the
  release bundle. Existing files are preserved unless replacement is
  requested; package-managed Linux plugins remain under package-manager
  ownership. Installation receipts describe files and request an OBS
  restart; they do not claim the module has loaded.
- Add Windows/macOS payload builds for the OBS plugin, FFmpeg, FFplay,
  libdatachannel and their non-system runtime dependencies and licenses.
  The software media path includes H.264, AV1 and Opus. Linux continues to
  use distribution-provided media libraries. Windows builds require all six
  H.264/AV1 encoder backends for NVIDIA NVENC, AMD AMF and Intel QSV; macOS
  enables VideoToolbox. Compiled encoder availability is recorded separately
  from execution on a compatible device and driver.
- Add native packaging gates for archive identity, bundled-file hashes,
  software media round trips and a private Velopack download/apply/restart
  probe. The probe updates a uniquely named fixture, then checks the new
  process version and executable hash; helper launch alone is not a pass.
- Refresh the development lock to the public zuse-computer 0.5.0 source.
  Registry staging continues to require a matching published package and
  complete source verification.

The recorded Linux integration gate at `535d6f36` passed 116 tests with the
native GUI backend linked. That snapshot precedes the zuse 0.5.0 dependency
refresh and is not evidence of Windows/macOS execution. The new installer,
window, media and OBS gates need their own platform receipts; see
[VALIDATION.md](docs/VALIDATION.md) and [UPDATES.md](docs/UPDATES.md).
Real Bilibili account/broadcast flows, two-party PK and physical GPU encoding
have not been validated in this development cycle.

The default packaging configuration does not provide Windows Authenticode
signatures or Apple Developer ID/notarization. macOS application and runtime
files use ad-hoc signatures; its installer package remains unsigned. Optional
macOS signing inputs are supported by the packaging script, and an actual release's
receipt records which signing steps were used.

## 0.1.2

- Use a warm-gray and terracotta-orange desktop palette while preserving
  the existing layout and operation behavior.
- Skip automatic OBS observer requests during read-only desktop startup,
  and avoid sending a stop request before observation has been enabled.
- Report automatic OBS streaming failures after a successful room start as
  partial failures. Preserve the live room state, stop later zuse mutations,
  and show guidance for checking OBS without repeating the room start.
- Show failed embedded zuse runs as failures in the desktop, retaining their
  execution receipts without retrying operations.
- Refresh zuse-computer to the 0.4.1 default-branch source, including its
  isolated Num Lock regression fixtures. Its runtime source is unchanged.

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
