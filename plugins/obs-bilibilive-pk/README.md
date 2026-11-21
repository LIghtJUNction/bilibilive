# Bilibilive OBS PK capture

This native OBS plugin sends the current program video and 48 kHz stereo PCM to
bilibilive through a private local connection. Video frames use a locked shared
memory mailbox; audio uses the SLPK v2 stream. The protocol is compatible with
StartLive's capture plugin.

## Build

Use the current installed OBS development SDK, SIMDe headers, CMake and Qt 6 Widgets. On Arch Linux these build prerequisites are provided by `obs-studio`, `simde`, `cmake` and `qt6-base`. The OBS SDK package exports the SIMDe dependency through `libobsConfig.cmake`:

```sh
cmake -S plugins/obs-bilibilive-pk -B dist/obs-plugin \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
cmake --build dist/obs-plugin --parallel
```

On Linux the module is `dist/obs-plugin/obs-bilibilive-pk.so`. Install it into
OBS's plugin directory, restart OBS, and enable **Bilibilive PK capture** under
**Tools**. A selected capture stream is negotiated before resources are
allocated. Only one media consumer connects at a time.

The native Rust media worker reads BGRA or NV12 video, feeds the selected FFmpeg
H.264 or AV1 encoder and Opus audio encoder, then sends RTP through the active
libdatachannel DTLS/SRTP session. HDR output requires an explicit SDR conversion
in OBS before capture. The capture ceiling is 60 FPS.

Linux/macOS use a private socket under `/tmp/startlive-pk-<uid>/`; Windows uses
`\\.\pipe\StartLive.PkMedia.v2`. These compatibility names are intentional.
Do not load both this plugin and the upstream StartLive PK plugin in one OBS
instance because they use the same capture endpoints and source identifiers.

See [UPSTREAM.md](UPSTREAM.md) for copyright and source provenance.
