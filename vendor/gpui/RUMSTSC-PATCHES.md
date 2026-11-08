# Local GPUI 0.2.2 patch

Source: crates.io gpui 0.2.2, Apache-2.0 (LICENSE-APACHE).
The upstream source and license are preserved. This repository adds a validated,
reference-counted CustomCursor API, with OS cursor implementations for Windows,
X11 and Wayland. Custom cursor styles participate in normal GPUI hit testing,
so local controls and window borders retain their normal cursors. No RDP code is
added to GPUI. See the root README for validation commands.

Two upstream ambiguous float literals in taffy.rs are explicitly typed as f32 for current Rust compatibility.

Seven upstream Metal shader indentation lines have whitespace-only normalization.
