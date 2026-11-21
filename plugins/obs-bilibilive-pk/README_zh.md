# Bilibilive OBS PK 采集插件

插件把 OBS 当前节目画面和 48 kHz 双声道 PCM 交给 bilibilive。视频通过带锁的共享内存传输，音频通过 SLPK v2 本地连接传输，与 StartLive 的采集协议兼容。

使用当前安装的 OBS 开发 SDK、SIMDe 头文件、CMake 和 Qt 6 Widgets 构建。Arch Linux 对应的构建依赖为 `obs-studio`、`simde`、`cmake` 和 `qt6-base`。OBS SDK 的 `libobsConfig.cmake` 会检查 SIMDe：

```sh
cmake -S plugins/obs-bilibilive-pk -B dist/obs-plugin \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
cmake --build dist/obs-plugin --parallel
```

Linux 产物为 `dist/obs-plugin/obs-bilibilive-pk.so`。放入 OBS 插件目录后重启 OBS，在「工具」菜单打开「Bilibilive PK 采集」。同一时间允许一个媒体消费者连接；连接时先协商需要的视频和音频流。

Rust 媒体进程读取 BGRA/NV12 图像，调用所选 FFmpeg H.264/AV1 与 Opus 编码器，再通过 libdatachannel 的 DTLS/SRTP 会话发送。HDR 输出需先在 OBS 中明确转换成 SDR。采集上限为 60 FPS。

本地 socket、共享内存和 Windows 命名管道保留 StartLive 的兼容名称。请勿同时加载本插件和上游 StartLive PK 插件，以免争用相同的采集端点和源标识。

源代码归属和许可证见 [UPSTREAM.md](UPSTREAM.md)。
