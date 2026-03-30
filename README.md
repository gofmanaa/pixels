# Rust Terminal Camera Viewer

A high-performance terminal-based live camera viewer written in Rust using ratatui for rendering, v4l for camera capture, and rayon for parallel processing. Supports multiple render modes, color filters, and optional bilinear sampling for smoother output.

### Features
- Half-block mode: Render camera feed using Unicode half-block `▀` characters for high-resolution terminal display.
- ASCII mode: Map camera feed to ASCII characters while retaining color.
- Filters: Apply real-time color filters (Normal, Grayscale, Inverted, etc.).
- Bilinear Sampling: Toggle smooth interpolation with the s key (Anti-aliasing).
- Screenshot (ANSI capture). Save the current frame with full terminal colors.
- Keyboard controls:
- - `q` - Quit
- - `a` - Toggle render mode (Half-block ↔ ASCII)
- - `s` - Toggle bilinear sampling
- - `1..0` - Change active filter
- - `p` - Save screenshot, save "image" to file frame_{ts}.ansi
- - `Space` - Toggle pause


### Dependencies
`v4l` – Camera capture
`ratatui` – Terminal UI rendering
`rayon` – Parallel processing
`color-eyre` – Error reporting

### Usege

```
cargo run --release

or 

cargo run --release
```

### Screenshot Output

Press `p` to save the current frame:
```
frame_1711829382.ansi
```

- Preserves full terminal colors

Can be viewed with:

```
cat frame_*.ansi
```

Or restored with color using:
```
less -R frame_*.ansi
```



### Notes
High resolutions may require a large terminal buffer and might impact performance.

Bilinear sampling provides smoother images but is slightly slower than nearest-neighbor.

Screenshot files are ANSI-encoded, not PNG/JPEG
