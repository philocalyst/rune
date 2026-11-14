# Standalone native first light

This GPUI adapter paints Rune's immutable display protocol in a native canvas.
It depends only on the existing `rune-display` crate and official **GPUI =0.2.2**.
It deliberately has its own workspace/lockfile: the native GUI dependency graph
does not enter the interpreter's normal workspace build.

## Verification

- This exact minimal library and original first-light example pass
  `cargo check --locked --example first_light` against the display source in
  Rune commit `e6dfe1d`, with unchanged GPUI default X11+Wayland features.
- The renderer implementation and first-light example also linked successfully
  in the fuller native staging package against the same official dependency
  versions. Native linker libraries were supplied from locally extracted matched
  official Debian development/runtime packages, without system changes.
- A real window smoke was attempted, but Xorg was blocked before application
  launch: this executor forbids Unix socket creation (`EPERM`). No native window,
  screenshot, actual font geometry, keyboard/mouse behavior or close behavior has
  therefore been verified. A successfully linked binary is not a window test.

The lockfile retains the remaining versions/checksums from GPUI's published
lockfile. Only local `rune-display` and `rune-gpui` entries were added. No community
GPUI fork, dummy renderer, disabled native backend or editor API is substituted.

## Build on a supported native development system

From the repository root:

```
cargo check --locked --manifest-path crates/rune-gpui/Cargo.toml --example first_light
cargo run --locked --manifest-path crates/rune-gpui/Cargo.toml --example first_light
```

The run command needs an actual supported display/graphics environment and native
linker dependencies. Linux requires usable XCB/xkbcommon/xkbcommon-x11 linker
libraries; installed runtime `.so.0` files alone need not provide development
linker names. Fontconfig/Wayland use dynamic loading; FreeType can use its bundled
C++ fallback. Follow actual compiler/linker errors when provisioning dependencies.

## What this fixture does

- Measures a native monospace font on the UI thread and uses its metrics for
  snapshot layout and paint.
- Paints the immutable fixture with clipped backgrounds, glyphs and cursor.
- Coalesces frame publication and capacity wakes, without idle polling.
- Relayouts on resize and moves the cursor on revision-scoped source hits.
- Returns undelivered input on shutdown, explicitly reports saturation/rejection,
  and quits the application when its final window closes.

It is not an editing UI: unsupported key and scroll events are reported by the
fixture host. Scalar/expansion-cluster painting is not full complex shaping.
There is no Lisp runtime/GC integration, editor-backed example, platform IME
InputHandler, preedit or composition transport in this minimal package. Those
remain separately staged and must not be inferred from this fixture.
