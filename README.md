# rumstsc

<img src="assets/rumstsc.svg" width="64" height="64" alt="rumstsc icon">

A compact native Rust + GPUI remote desktop application, designed for Linux and inspired by Windows Remote Desktop Connection. Dark and light palettes, a connection form, recent hosts, desktop size presets, and a scaled remote framebuffer. No webview, Electron, FreeRDP, rdesktop, IronRDP, or external RDP process.

**This is an experimental RDP client, not a complete MSTSC replacement.** It supports verified TLS 1.2/1.3, NLA using CredSSP v5/v6 and NTLMv2, pinned legacy RSA/RC4 security, classic compressed bitmap graphics, bitmap caching, and an RDPGFX 8.1/AVC420 path. Linux and real xrdp login-screen interoperability have been tested. NLA is covered by protocol vectors and a TLS/CredSSP peer test; authentication against a real Windows server remains unverified.

## Install

### Debian / Ubuntu (apt)

```sh
echo "deb [trusted=yes] https://cattens-roses.github.io/rumstsc ./" \
  | sudo tee /etc/apt/sources.list.d/rumstsc.list
sudo apt update
sudo apt install rumstsc
```

Later upgrades:

```sh
sudo apt update
sudo apt upgrade rumstsc
```

Continuous package versions look like `0.1.0+<build>.git.<sha>` so each `main` publish sorts newer for apt.

Then run:

```sh
rumstsc
```

Package metadata and install instructions are also published at [cattens-roses.github.io/rumstsc](https://cattens-roses.github.io/rumstsc/).

### GitHub Releases

Every push to `main` updates a [continuous prerelease](https://github.com/Cattens-Roses/rumstsc/releases/tag/continuous) with the latest `.deb` and Windows zip. Push a tag `v*` (for example `v0.1.0`) for a stable release with generated notes.

## Build and run

Use a current stable Rust toolchain (edition 2024). On Debian/Ubuntu, install GPUI's native build dependencies:

```sh
sudo apt-get install build-essential clang cmake pkg-config \
  libfontconfig1-dev libfreetype6-dev libxkbcommon-x11-dev \
  libwayland-dev libxcb1-dev libx11-xcb-dev libssl-dev libvulkan-dev ffmpeg
cargo run --locked --release
```

A graphical Wayland or X11 session and a working Vulkan graphics driver are required on Linux. Both backends are enabled in GPUI. Windows builds require the MSVC Rust toolchain and Visual Studio C++ build tools. AVC420 uses Windows Media Foundation on Windows; Linux dynamically loads system FFmpeg 6/7/8 libraries (`libavcodec`, `libavutil`, `libswscale`). No FFmpeg executable or subprocess is started by the client.

The system dependencies follow [GPUI/Zed's Linux build guidance](https://github.com/zed-industries/zed/blob/main/docs/src/development/linux.md); this app does not require Zed's database, media, or Git development libraries.

To build only the engine and headless probe (without GPUI):

```sh
cargo run --locked --no-default-features --bin rumstsc-probe -- localhost:3389
cargo test --locked -p rumstsc-rdp
```

By default the probe negotiates security and verifies TLS, then disconnects without authentication. `--session` explicitly starts a desktop session and waits for bitmap updates. `--output frame.ppm` captures a settled framebuffer. Run `--help` for the security, certificate pin, user and domain arguments. For NLA, the probe reads the password from `RUMSTSC_PASSWORD`; passwords are never command-line arguments. Probe success without `--session` does not imply successful authentication or a usable desktop.

Keep `Cargo.lock` and use `--locked`. It pins libc 0.2.186 because GPUI 0.2.2's older xattr dependency uses a Linux constant removed in later libc releases.

## Use

1. Enter a hostname, IPv4 address, or `[IPv6]:port`. Port defaults to 3389.
2. Enter a username and password for NLA. `DOMAIN\user` is accepted, or enter a domain separately in **Show options**. Password fields are masked, cannot be copied, are cleared after connection starts, and are never saved.
3. Select **Connect**, or press Enter. Tab and Shift+Tab move through the text fields.
4. **TLS / NLA** is the default. **NLA only** requires CredSSP; **TLS only** allows the remote login screen without NLA; **Legacy** explicitly selects Standard RDP security. There are no retry downgrades. TLS uses system trust, or an explicit SHA-256 certificate fingerprint from a trusted source. Legacy requires the SHA-256 fingerprint of the RDP server certificate blob, reported by `rumstsc-probe --mode legacy HOST`. A pin replaces chain/hostname validation, while TLS handshake signature verification remains enabled.
5. If system certificate validation fails (for example, `UnknownIssuer` on a private or self-signed certificate), the GUI pauses before authentication and shows the server, subject, issuer, validation error and full SHA-256 fingerprint. Compare the fingerprint with the server administrator through a trusted channel, then select **Trust once and connect**, or **Cancel**. Approval applies only to the certificate in that handshake and is never saved; it overrides certificate identity/name/validity checks for that connection while retaining TLS handshake signature verification. An explicit pin mismatch cannot be overridden by this review. The headless probe continues to require system trust or `--pin`.
6. When activated, click the desktop to capture input. Ctrl+Alt+Home releases held keys and mouse buttons and returns input to the local app. Losing window activation also releases input. Use **Disconnect** to close the socket.

The header switches light/dark themes. Only theme, resolution, display preference, and up to five successfully activated hosts are saved under `$XDG_CONFIG_HOME/rumstsc/settings` or `~/.config/rumstsc/settings` (`%APPDATA%/rumstsc/settings` on Windows). Usernames, passwords and certificate pins are not persisted. Legacy RC4 is obsolete; prefer TLS/NLA. The plaintext option exists only for isolated protocol tests, resets on restart, and refuses local password submission. Remote keystrokes in that mode are still plaintext.

## Implemented protocol subset

The `crates/rdp` crate forbids unsafe Rust in its own code and implements RDP messages and state handling here, based on [MS-RDPBCGR](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/023f1e69-cfe8-4ee6-9ee0-7e759fb4e4ee), [MS-CSSP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-cssp/385a7489-d46b-464c-b224-f7340e308a5c), [MS-NLMP](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/b38c36ed-2804-4868-a9ff-8dd3182128e4), and MS-RDPELE:

- TCP, TPKT and X.224 connection/data packets; explicit security rejection without retry downgrade.
- BER MCS Connect Initial/Response, GCC client core/security/network blocks, user attachment and channel joins.
- TLS with native trust roots or explicit certificate pinning; no accept-all verifier.
- CredSSP v5/v6 with NTLMv2 proof, Authenticate MIC, channel bindings, signed/sealed public-key binding verification before credential delegation, and signed/sealed password credentials. CredSSP versions below v5 and weak NTLM negotiations are refused.
- Standard RDP RSA random exchange, 40/56/128-bit RC4, plain/salted MAC verification and separate directional keys with 4,096-packet rotation. Proprietary and X.509 RDP certificates require an explicit pin. FIPS is unsupported.
- Unicode Client Info, license request/challenge/new-license handling with MAC checks, valid-client alerts, Demand/Confirm Active capability exchange, synchronization, control and font finalization. Licenses are kept only for the current exchange, not stored for reuse.
- Incremental TCP reassembly, multiple Share PDUs per packet, desktop reactivation and bounded receive/send/input buffers.
- MPPC bulk decompression with 8K/64K history, reset/front handling, overlapping references and bounded output; negotiated in Client Info. History is shared across Share Data PDUs. No external compression or RDP library.
- Slow-path raw bitmap rectangles at 15/16/24/32 bpp and interleaved RLE at 15/16/24 bpp, with optional compression headers, all regular/lite/mega/special orders, bottom-up rows, checked run lengths and BGRA output. RDP6 planar decoding handles 32-bit compressed bitmaps. Classic-only connections request 24-bit color; a negotiated graphics channel requests 32-bit color.
- Primary ScrBlt screen copies with retained fields, signed deltas, clipping, overlap-safe copies and all 16 source/destination ROP3 operations. Alternate frame markers keep incomplete logical frames off the display, including when an END and the next START share one Orders PDU. Primary MemBlt and Revision 1 cache uploads reuse session-local bitmaps; three bounded caches hold 120/120/512 entries with 768/3072/12288-byte cells. Cache contents survive desktop reactivation. Other classic drawing orders and offscreen caches are not advertised.
- Slow-path set-1 keyboard input (US layout), modifiers, three mouse buttons, movement and vertical wheel input.

The RDP wire protocol, NTLM/CredSSP messages, legacy packet security, licensing and RLE decoder are implemented in this repository. With user-approved TLS/cryptography dependencies, rustls handles TLS, RustCrypto provides hashes/HMAC/RSA, and supporting crates provide random bytes, X.509 parsing, constant-time comparison and secret clearing. No RDP or CredSSP library is used. The GUI uses GPUI and its existing image pixel container. User-approved system video codecs live in `crates/video`; the RDP parser still forbids unsafe code. Native OS presentation is isolated in `crates/display`.

Not implemented: Kerberos/Remote Credential Guard, CredSSP versions before v5, SPNEGO mechanism-list MIC exchanges, FIPS/3DES, persistent CAL storage, drawing orders other than ScrBlt/MemBlt, bitmap cache revisions 2/3, legacy RemoteFX CAVIDEO, AVC444/AVC444v2, fast-path output and RDP6/RDP6.1 bulk compression, Unicode remote keyboard input, clipboard/drive/audio redirection, gateways, UDP, multiple monitors and reconnect/resumption. Classic capability sets advertise only implemented drawing orders. The graphics channel selects version 8.1 with AVC420; RemoteFX Progressive is supported through WireToSurface2; legacy CAVIDEO remains unsupported. Unsupported packets fail explicitly; the app never launches an external RDP client as a fallback. NLA environments that prohibit NTLM require Kerberos support and cannot authenticate with this client yet.

## Validation

```sh
cargo fmt --all --check
cargo test --locked -p rumstsc-rdp
cargo test --locked --bin rumstsc
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Tests cover wire truncation, channel routing, pixel orientation/padding, compressed rectangles, RLE opcodes, keyboard input, cancellation and negotiation policy. Crypto tests include a published NTLMv2 response-key vector and Microsoft sealing vector, tamper/replay rejection, and legacy key rotation. TLS tests reject untrusted private certificates and incorrect pins, gate application bytes on explicit certificate approval, and exercise declining review and worker cancellation. A local TLS/CredSSP peer verifies NTLM proof and MIC, both binding hashes and delegated credentials, and confirms a bad server binding prevents delegation.

All 62 workspace tests pass on Windows and Ubuntu 24.04 WSL. The app links on Linux, and the X11 interface renders both themes on a virtual display with Mesa software Vulkan. Real TLS 1.3 and pinned RSA/128-bit RC4 sessions against xrdp 0.9.24 capture its 1280 × 720 login screen; the TLS capture decoded 221 compressed rectangles. This verifies the login-screen path, not an authenticated user's entire desktop workflow. Windows NLA interoperability, hardware GPU rendering and Wayland interaction remain unverified. No binary size or runtime memory claims have been measured.

Reproduce the isolated xrdp test on Debian/Ubuntu:

```sh
bash scripts/prepare-xrdp.sh
CARGO_TARGET_DIR=.interop/target-linux cargo build --locked --no-default-features --bin rumstsc-probe
sudo bash scripts/xrdp-smoke.sh tls
sudo bash scripts/xrdp-smoke.sh legacy
```

The preparation script unpacks the distro package under ignored `.interop/`, without installing a system service. The smoke test supplies xrdp's hard-coded resource paths in a private mount namespace, binds loopback port 33991, and stops its process on exit. It never changes the host's xrdp configuration. Frame captures and temporary test keys stay under `.interop/`.

## Structure

- `crates/rdp/`: wire engine, TLS/crypto integration, CredSSP/NTLMv2, licensing, RLE and network worker.
- `src/`: GPUI interface, native editable fields, themes and preferences.
- `src/probe.rs`: headless security/settings probe.
- `assets/rumstsc.desktop`: optional Linux desktop launcher.
- `.github/workflows/ci.yml`: core tests on Linux/Windows, Linux GUI build.
- `.github/workflows/release.yml`: builds the `.deb` and publishes the apt repo to GitHub Pages.

The network worker and graphics decoder run on separate threads. A bounded queue holds at most eight ordered bitmap updates; it applies backpressure instead of dropping incremental updates. Snapshot copies also run on the graphics worker, and the UI consumes completed snapshots without taking the framebuffer lock. Frame updates are coalesced into a shared framebuffer; the UI checks for changes every 16 ms and uploads only modified 128 x 128 pixel tiles, skips a busy decoder instead of blocking, and evicts retired GPU images. Per-tile revisions preserve changes across skipped ticks, and framebuffer generations invalidate the tile cache after server reactivation or resizing. The release profile uses thin LTO, a single codegen unit, stripped symbols and abort-on-panic. The application does not poll while disconnected.


## Graphics throughput

The receive loop drains queued TCP/TLS records continuously and sleeps only when neither reading nor writing makes progress. It checks input and cancellation between reads. This replaces the previous unconditional 8 ms delay after each 16 KiB read, which throttled graphics reception even on fast connections. The receive scratch buffer is reused and is now 64 KiB.

A loopback test sending 320 raw bitmap updates (7,390,400 wire bytes) improved from 5.506 s (1.3 MiB/s) to 0.298 s (23.7 MiB/s) on the Windows development build, before any release optimizations. This measures transport plus bitmap decoding, not displayed FPS or performance over a real RDP connection. It does not establish parity with MSTSC's newer graphics codecs.

Reproduce the optional throughput measurement (excluded from normal CI because wall-clock timing depends on host load):

```sh
cargo test --locked -p rumstsc-rdp --test session_transcript bulk_bitmap_receive_throughput -- --ignored --nocapture
```

Use `cargo run --locked --release` for interactive sessions; debug builds leave pixel conversion and RLE decoding unoptimized.

Small screen changes no longer clone or upload the entire framebuffer. For a 1920 x 1080 desktop, a change contained in one full tile copies 65,536 bytes instead of 8,294,400 bytes. This is a reduction in CPU copy and GPU upload data, not a measured increase in displayed FPS. Full-screen changes still update every tile, and the current classic bitmap protocol remains less capable than newer RDP graphics modes.


## Loading traces

Tracing is disabled by default, including per-packet timing collection and writer threads. Set `RUMSTSC_TRACE=1` before starting the GUI to record a bounded Chrome Trace JSON file for startup (`startup-trace.json`) and the latest connection (`last-load-trace.json`) alongside its settings. On Windows this is normally `%APPDATA%/rumstsc/`; on Linux it is `$XDG_CONFIG_HOME/rumstsc/` or `~/.config/rumstsc/`. Exports run on a separate writer for the first 30 seconds, and the connection trace is saved again when the session ends. The first 32,768 events are retained. Each new connection replaces the previous connection trace. Exports write a sibling temporary file and atomically replace the completed JSON so interruption cannot truncate the last successful export.

The trace contains fixed operation names, relative times, durations and numeric counts. It excludes endpoints, certificate contents, usernames, passwords, keystrokes and screen pixels. Network, graphics and UI use separate trace lanes. Startup timings include GPUI initialization, window creation and the first view render. Connection timings cover DNS, TCP, RDP negotiation, native trust store loading, TLS, certificate review waiting, NLA, MCS, channel joins, incoming packet processing, bitmap queue age and decode time, background snapshots, UI image preparation and first frame submission. `tile_upload_submit` measures the CPU-side graphics submission call, not GPU completion or the instant the monitor displays the frame. A bitmap decode event is not a complete-frame marker; the server may send many incremental rectangles.

The headless probe also accepts `--trace FILE.json` with `--session`. The isolated xrdp smoke script saves `trace-tls.json` or `trace-legacy.json`. This makes network waiting distinguishable from decoder work and certificate approval time. Keep comparisons on the same server, security mode, resolution, network and build profile; a local xrdp trace alone does not establish the reason for a difference from MSTSC on a Windows server.

In one loopback Windows release test, 221 compressed rectangles were decoded in about 7 ms of accumulated decoder work, with the first bitmap decoded about 219 ms after connection start. Several handshake intervals were around 40 ms; these were waiting intervals, not decoding time. The two independent initial MCS requests are now sent in one TLS write while retaining their separate TPKT boundaries and order. The local debug Linux GUI trace separately exposed about 0.8 s for GPUI/window initialization and an explicit two-second certificate approval wait in the test driver. These timings describe those fixtures, not the user's server or an MSTSC benchmark.

Three repeat Windows release traces after batching measured channel attachment/joins at 2.40-2.64 ms, compared with 47.76 ms in the earlier local trace. First-bitmap decode occurred at 170-193 ms after connection start (earlier trace: 219 ms); accumulated initial decoder work was 6.2-6.3 ms. Other handshake waiting intervals remained around 40-50 ms. The asynchronous burst test processed the same 320-update workload in 0.100 s on Windows, compared with 0.298 s before separating the decoder. These are fixture observations, not end-to-end MSTSC or remote-server comparisons.


## Scrolling during an active session

The client now advertises ScrBlt and alternate frame markers using the Order Capability Set layout from [MS-RDPBCGR](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/9f409c29-480c-4751-9665-510b8ffff294). Previously its order capability flag fields were at incorrect offsets and no screen-copy support was advertised. ScrBlt lets the server move pixels already present on the client, sending new pixels only for exposed regions; the wire operation for a 1920 x 1000 copy is 22 bytes before enclosing Share/MCS/TPKT/TLS headers. This is not a claim that an entire scrolling update is always 22 bytes: newly exposed content still needs graphics updates, and the server chooses the operations.

When a server sends logical frame markers, the graphics worker withholds partial snapshots until END, captures the completed image before any following START, and continues decoding/receiving in the background. This prevents displaying intermediate stripes within a marked frame. The client cannot infer reliable frame boundaries for unmarked bitmap streams, and it does not hide a slow transfer behind a long arbitrary debounce delay.

Tests cover the drawing-order wire state, every supported raster truth table, four-way/diagonal overlap, clipping, bounds reuse and deltas, truncation, bitmap/order queue ordering, and atomic marked-frame snapshots. The trace records `screen_orders_decode`, `screen_blits` and `logical_frames` so a real scrolling session can show whether the server uses these features. Servers or browser rendering paths that continue to send only bitmap strips still need more bandwidth and do not gain the screen-copy optimization; the new AVC420 path depends on server negotiation.

Live Linux/xrdp login-dialog movement still rendered correctly with the new capability layout. That fixture emitted bitmap updates, not ScrBlt or logical frame markers; it therefore validates the bitmap fallback, not real-server use of the new optimizations. ScrBlt/frame behavior is covered by an independently constructed local RDP transcript and decoder tests. Real Windows/browser scrolling remains to be verified.

## Scrolling bandwidth and background work

The graphics worker blocks on its bounded command queue instead of polling every 2 ms. Snapshot requests share that queue, only wake it after graphics changes, and allow at most one queued request and one outstanding response. Complete-frame callbacks skip pixel copying while a response is waiting for the UI. Normal connections collect no trace entries and start no trace writer threads. Network socket readiness still uses nonblocking polling; this change does not make all application threads event driven.

MPPC 8K/64K is negotiated in Client Info and decoded before Share Data dispatch. Tests cover high/low literals, all offset and length classes, shared packet history, overlapping copies, ring sources, resets, bounds and malformed tokens. Protocol-peer tests exercise both compression types through activation, repeated bitmap updates, screen copying and keyboard input. Real xrdp checks use `bulk_compression=true` by default; set `RUMSTSC_TEST_BULK=false` when running `scripts/xrdp-smoke.sh` for a local bandwidth control. Opt-in traces expose `bulk_compressed_bytes`, `bulk_expanded_bytes`, `raw_rectangles` and `rle_rectangles`.

The complete entries recovered from a previously interrupted user trace contained about 21.3 MB of network input over 14.7 seconds, about 10.5 ms of accumulated bitmap decode spans, and no screen-copy orders or frame markers. These spans are elapsed instrumentation intervals, not process CPU measurements or displayed-frame latency. This evidence supports investigating wire efficiency; it does not prove whether server generation, bandwidth or display presentation dominates that session. RDPGFX/AVC420 and bitmap caching have since been added; this trace predates them and does not establish their effect or MSTSC performance/resource parity. No same-workload MSTSC CPU/RAM comparison has been completed.

In an isolated xrdp 0.9.24 TLS login-screen control, enabling MPPC reduced session-loop input from 84,747 to 41,469 bytes (about 51%). The decoder expanded 39,118 compressed bytes into 82,387 bytes across 44 PDUs. Both runs produced exactly identical 1280x720 PPM pixels (221 bitmap-RLE rectangles); pinned legacy RC4 also decoded the compressed stream successfully. This is an initial-screen bandwidth comparison, not a scroll FPS, CPU/RAM or Windows MSTSC benchmark.

The updated Linux GPUI application also completed local TLS/MPPC rendering and interactive login-dialog redraws on Xvfb/software Vulkan. The optimized Windows probe independently decoded the same real xrdp MPPC desktop over pinned TLS. Seventy automated tests passed on each platform (one optional throughput benchmark remains ignored). These checks do not cover a real Windows server's scrolling workload.

## RDPGFX, AVC420 and native display

RDPGFX is negotiated automatically when the server supports extended client data and a system H.264 decoder is available. The client requests `drdynvc`, negotiates DVC v1/v2, reassembles static/dynamic channel fragments, decodes RDP8 segmented compression, advertises graphics version 8.1 with AVC420, and manages surfaces, output mappings, fills, surface copies, a 16 MB graphics cache and ordered frame acknowledgements. ClearCodec includes residual runs, glyphs, V-Bar dictionaries, raw/RLEX/NSCodec subrectangles; planar color/RLE is also implemented. The RDPGFX wire implementation is in this repository; Media Foundation/FFmpeg decode only H.264. Persistent video decoder state is retained per surface, and only rectangles in the AVC region mask update the desktop. AVC444 and the legacy RemoteFX CAVIDEO codec are not implemented. Set `RUMSTSC_GFX=0` to request the classic bitmap/cache path for a compatibility check.

In **Show options**, select **Display: native framebuffer** before connecting to keep GPUI responsible for controls and draw the desktop in a native child window instead. Windows uses GDI; Linux uses X11 (including XWayland when GPUI itself runs on X11). Set `RUMSTSC_RENDERER=native` or `RUMSTSC_RENDERER=gpui` for a temporary override. Native Wayland subsurfaces are not implemented; unavailable native presentation falls back to GPUI. The footer shows the actual renderer and, after frames arrive, the negotiated graphics path. Keyboard/mouse input and Ctrl+Alt+Home work through the child window, and focus loss releases held inputs.

The native path merges pending tile deltas, uploads to a separate framebuffer and presents a complete image. X11 uploads into an offscreen pixmap before one screen copy, so network strips are not shown as partially uploaded native frames. It blocks on native events when idle. It avoids GPUI RenderImage/atlas uploads for desktop pixels; GPUI still initializes its own graphics resources for the application controls. This is not GPU video decoding or zero-copy presentation. The default renderer remains GPUI until measurements on the target desktop justify changing it.

Trace entries `rdpgfx_decode`, `rdpgfx_frames`, `avc420_updates`, `clearcodec_updates`, `planar_updates` and `gfx_raw_updates` identify the selected protocol work. `native_tile_apply` and `native_present_submit` measure native CPU-side work; neither proves display completion. Tests independently assemble a full MCS/drdynvc/RDPGFX/AVC session, including IDR/P pictures, masked updates, cache reuse, surface copies and acknowledgements, on Windows Media Foundation and Linux FFmpeg. Additional tests check full-range BT.709 conversion, fragment bounds, ClearCodec/planar vectors, bitmap-cache replacement/reactivation, tile coalescing and input release. Native GDI pixels are verified in a memory DC; native X11 pixels and legacy-title parent lookup are verified under Xvfb with a 32-bit parent visual. Real Windows-server AVC interoperability and same-workload MSTSC FPS/CPU/RAM comparisons remain unverified.

The earlier graphics validation passed 88 automated tests on Windows and Linux (one optional throughput test is ignored), including native X11 presentation under Xvfb. An isolated real xrdp TLS session also exercised the complete Linux native GUI: typing and dragging the login dialog succeeded, and its trace recorded native presentations with zero GPUI desktop-image preparation/upload events. Pinned legacy RC4 bitmap rendering remains verified. These checks validate implementation paths; they do not establish real Windows-server AVC interoperability or performance parity with MSTSC.


RDPGFX negotiation now honors the server's `EXTENDED_CLIENT_DATA_SUPPORTED` negotiation flag, requests `CS_MCS_MSGCHANNEL`, joins the assigned message channel, and advertises network-characteristics detection alongside the graphics-pipeline flag. RTT and connect-time/continuous bandwidth requests receive protocol responses on that channel, including the required security header under TLS. The client previously advertised graphics without this required prerequisite. The virtual-channel capability now declares a valid 1600-byte chunk size. The protocol transcript test withholds licensing/AVC until message-channel probes receive valid replies, then completes the desktop on the same connection. Automatic graphics retry/reconnection has been removed; credentials are cleared immediately after Client Info again. These protocol checks do not substitute for verifying the user's real Windows server.

The real-server first-desktop stall was reproduced through the CLI: the server sent a DVC v2 capabilities request, the client replied v1, and no graphics channel was created. The client now selects v2 when offered v2/v3 (v3 compression is not advertised), while retaining compatibility with v1 peers. With that correction, the same pinned TLS server opened RDPGFX and delivered its 1280x720 desktop on the original connection. The protocol transcript now requires the v2 response before opening graphics; unit tests also cover priority-class create requests and v3-to-v2 selection.

The final optimized Windows probe and the Linux probe both received the real remote server's 1280x720 RDPGFX desktop with the approved certificate pin. The Windows workspace passed 91 tests, and Linux protocol tests/clippy passed, including the updated v2 handshake regression. This verifies desktop startup through the CLI; it is not an interactive scrolling benchmark.

A subsequent real-server check found planar color updates carrying a zero alpha plane: the decoded RGB desktop was correct, but all pixels reached GPUI as transparent. GFX color updates now keep desktop pixels opaque instead of applying the codec alpha plane (MS-RDPEGFX 3.3.5.1). A regression test checks both surface pixel formats through frame composition and tile snapshots. The probe now reconstructs the same snapshots consumed by the GUI and prints their alpha range; the real-server result changed from 0..0 to 255..255 with identical RGB pixels.

The corrected GPUI application was also visually checked against the same remote server in isolated Linux/Xvfb/software Vulkan: the xrdp login dialog and desktop background were visible, rather than transparent/black. No remote login credentials were entered.

`RDPGFX_WIRE_TO_SURFACE_PDU_2` (0x0002) now has per-surface/per-ID progressive decoder contexts and matching DeleteEncodingContext handling. RemoteFX Progressive decoding is implemented here without a codec/RDP library: RLGR1, SRL/raw progressive upgrades, differential coefficient history, original and reduced-boundary inverse wavelets, scalar quantization, region clipping, and opaque YCbCr output. The capability advertises SMALL_CACHE + AVC420, clearing THINCLIENT so progressive use is negotiated consistently. Contexts and coefficient history have explicit count/memory bounds. Tests cover published entropy/reference coefficients, independently encoded gradients/textures and difference frames, clipping/truncation, context deletion, and complete-frame acknowledgements. Trace counter `progressive_updates` identifies this path. An authenticated desktop login on the user's account still requires user verification; no account password was requested or saved during development.

### Remote pointer shapes

Slow-path system, color, new and cached pointer updates are decoded with a bounded 25-entry cache and validated masks/hotspots. The desktop uses the server-provided cursor, including window resize arrows and text carets. Native Windows and X11/XRender displays use OS cursor resources so movement does not repaint the framebuffer; GPUI draws the cursor separately from cached desktop tiles. Screen-XOR pixels use opaque color when the rendering API cannot express inversion. Large pointers and server-driven pointer repositioning are not advertised.

### Application icon

`rdpicon.svg` is the icon source, embedded directly in GPUI and tinted for either theme. Run `cargo run --locked --manifest-path scripts/icon-export/Cargo.toml` to regenerate the launcher SVG, transparent PNG sizes (16–512 px), multi-resolution Windows ICO, and X11 ARGB property data. Generated assets are committed so normal builds do not need the exporter. Windows embeds resource 1 for Explorer, the title bar and taskbar; Linux packages install the hicolor icons and desktop entry with matching application ID.

### Workspace preferences

Open **Settings** in the header for Appearance, Display, Performance and Privacy. Theme selection is available only there. Preferences save automatically and existing settings migrate with safe defaults. Display presets and renderer selection apply on the next connection; appearance, status bar and presentation limits apply immediately. The header's **Full screen / Windowed** button works before or during a session. F11 also toggles it when remote keyboard capture is released. An optional setting enters full screen after activation.

Presentation profiles cap UI refresh at approximately 60, 30 or 15 updates per second. Battery saver reduces snapshot requests, image uploads and UI wakeups; it does not skip protocol decoding or guarantee a particular battery-life improvement. Background presentation is paused by default while the app is unfocused or showing Settings. The network connection and decoder continue, and returning to the desktop requests accumulated changes. History can be disabled or cleared; credentials are never persisted.

UI code is separated into connection, desktop, chrome and preferences views under `src/app/`, shared controls in `src/ui.rs`, and persistence in `src/settings.rs`.

Full screen hides the application header and status bar, giving the desktop the complete monitor area. Move to the upper-right 32-pixel corner to reveal the controls; move back into the desktop to hide them. This also works with the native Windows/X11 renderer, without interrupting a remote mouse drag. Ctrl+Alt+Home still releases input and reveals controls; F11 or Escape exits full screen after releasing capture. Native presentation can upscale in full screen while preserving aspect ratio and input coordinates.
