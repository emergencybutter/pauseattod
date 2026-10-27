# Pause at ToD

An X-Plane 11.50+/12 plugin, written in Rust, that pauses the sim when a condition you've armed comes true. You can step away during cruise and come back to a paused aircraft at top of descent, near a waypoint, at an altitude, or after a set time.

## Using it

**Plugins → Pause at ToD → Settings...** opens a modern, resizable X-Plane floating window. It's drawn with [egui](https://github.com/emilk/egui) and follows X-Plane's UI scale:

```
╭─ Pause at ToD ──────────────────────────────────╮
│ ● ARMED                               [ Disarm ] │
│ ╭──────────────────────────────────────────────╮ │
│ │ (●) Top of descent   FMS T/D, 3:1 rule or ... │ │
│ │ Descent to [FMS dest] at [auto] ft            │ │
│ │ Pause [5 nm] before the top of descent        │ │
│ │ T/D in 38.2 nm (to KSEA at 1,933 ft) ...      │ │
│ │ ▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░                            │ │
│ ╰──────────────────────────────────────────────╯ │
│ ╭ (○) Near waypoint ───────────────────────────╮ │
│ ╭ (○) Altitude ────────────────────────────────╮ │
│ ╭ (○) Timer ───────────────────────────────────╮ │
╰─────────────────────────────────────────────────╯
```

- Switch on the conditions you want and press **Arm**. The first enabled condition that comes true pauses the sim. The plugin then disarms, and the window pops up with the reason.
- While you edit, the window looks up identifiers as you type, e.g. "KSEA · airport · 142 nm away · cross at 1,933 ft (field 433 + 1,500)".
- While armed, the inputs are locked and each active condition shows a live status line and a progress bar.
- **Plugins → Pause at ToD → Armed** shows the armed state and toggles it.
- You can bind keys to two commands: `pauseattod/toggle_window` and `pauseattod/toggle_arm`.
- Settings are saved in `X-Plane/Output/preferences/pauseattod.prf`.
- Loading a new flight or aircraft disarms the plugin.

### Conditions

| Condition | Fires when |
|---|---|
| **Near waypoint** | The great-circle distance to the waypoint is at or below N nm. Arming while already inside that distance fires straight away. |
| **Altitude** | You pass the altitude in the chosen direction (descending, climbing or either way), in MSL (pilot altimeter) or AGL. It only fires on an actual crossing: arming while already below a "descending" target doesn't fire. |
| **Top of descent** | The first of the three ToD heuristics below fires. |
| **Timer** | N minutes of sim time have passed since arming. Time spent paused doesn't count. |

**Top-of-descent heuristics.** All three are evaluated, and none of them run below 1,000 ft AGL:

1. **FMS value.** If the dataref named by `tod_dataref` exists and has reported a positive distance, the plugin pauses once that distance is at or below the lead. The default name is X-Plane 12's `sim/cockpit2/radios/indicators/fms_distance_to_tod_pilot` (float, nm), which the built-in FMS fills once it has a descent planned. If the dataref is missing or never goes positive, this heuristic stays inactive. You can point `tod_dataref` at an aircraft's own ToD dataref in the prefs file.
2. **Geometric 3:1 rule.** ToD is `(altitude − crossing altitude) / 1000 × 3 nm` before the end-of-descent point. The plugin pauses when the distance to go minus that figure is at or below the lead. The point defaults to the destination in X-Plane's built-in FMS. The crossing altitude defaults to the field elevation + 1,500 ft when the point is an airport. The elevation comes from the nav database, from the airport itself or from the FMS destination. For other kinds of point it defaults to 1,500 ft MSL. The SDK documents the nav height as feet, but X-Plane returns meters, so the plugin converts it and logs the result (`PauseAtToD: KSEA field elevation 433 ft (nav height 132.0 m)`). Values outside −1,500 to 15,000 ft are ignored.
3. **Descent onset (fallback).** The plugin first needs at least 60 s of level flight (|VS| < 300 fpm) above 5,000 ft AGL. After that, a descent of at least 500 fpm sustained for 10 s fires. This covers third-party FMSs (Zibo, ToLiss and others) that don't feed X-Plane's FMS, at the cost of pausing at the descent rather than before it.

**Waypoint lookup.** Entries in the loaded X-Plane FMS flight plan come first. Otherwise the plugin uses the nav-database airport, VOR, NDB or fix with exactly that ID that's closest to the aircraft. The SDK's fragment search is tried first, and a full exact-match scan runs only if that fails.

Advanced settings exist only in the prefs file: `tod_nm_per_kft` (default 3), `tod_dataref`, and `descent_vs_fpm` (default −500).

## Design

```
crates/core    pauseattod-core: pure Rust, no X-Plane. Settings + file format,
               great-circle math, and the Monitor that evaluates conditions
               against AircraftState samples. All heuristics are unit-tested.
crates/plugin  pauseattod (cdylib → win/lin/mac.xpl)
  ffi.rs       hand-written bindings for the ~40 SDK functions used, checked
               against the official SDK headers and import libraries
  xp.rs        safe wrappers: datarefs, commands, menu, nav/FMS lookups
  gui.rs       egui hosted in an XPLM floating window: input translation,
               keyboard focus, painting with egui_glow
  gl.rs        GL loader, boxel→pixel transform, GL state save/restore
  view.rs      the window's content (plain egui code)
  app.rs       state machine: arm/disarm, sampling, firing, previews, prefs
  lib.rs       XPlugin* entry points and C callbacks
xtask          `cargo xtask dist` packaging/installing, also in Rust
```

Key decisions:

- **No C toolchain or SDK download.** On Windows the SDK functions are linked through `raw-dylib` imports from `XPLM_64.dll` (and `opengl32.dll` for the GL loader), so no `.lib` files or bindgen/libclang are needed. On macOS the build uses `-undefined dynamic_lookup`. Linux resolves the symbols when X-Plane loads the plugin.
- **Sim logic is separate from the sim.** `core` gets plain samples (position, MSL/AGL altitude, VS, sim flight time, optional FMS ToD distance), so you can test every heuristic with `cargo test` on any machine.
- **UI: egui in a modern XPLM window.** The window uses X-Plane's own floating-window decoration (title bar, close, resize). Each draw callback runs an egui frame, maps boxels to pixels with the transform X-Plane set up, and paints with egui_glow on X-Plane's GL context. X-Plane 12's Vulkan build still offers that context to plugins. Every piece of GL state egui touches is saved before painting and restored after. Typing reaches the window only while a text field has focus, so sim key bindings keep working.
- **Re-entrancy safety.** Widget calls can call back into the plugin synchronously. Callbacks never touch the app directly: they post an `Event`, and one guarded pump (`with_app`) drains the queue. A re-entrant call queues its event and returns, so a `RefCell` double borrow can't happen.
- **Panics are contained.** Every `extern "C"` callback runs inside `catch_unwind`. A bug is written to `Log.txt` and doesn't crash X-Plane.
- **Edge detection keeps history.** Altitude crossings compare consecutive samples. A jump of more than 2,000 ft between samples is treated as a reposition and ignored. The timer restarts if the sim clock goes backwards (new flight). Conditions are only evaluated while the sim isn't paused, at 10 Hz.
- **Pausing** uses `sim/operation/pause_on`, falling back to `pause_toggle` only if the sim isn't already paused.

## Build and install

You need a stable Rust toolchain. Windows needs the MSVC target.

```sh
cargo test -p pauseattod-core                     # heuristics
cargo xtask dist                                  # → dist/PauseAtToD/64/win.xpl
cargo xtask dist --install "C:/X-Plane 12"        # also copies to Resources/plugins
```

Build `mac.xpl` and `lin.xpl` on those platforms with the same command, or pass `--target <triple>`. Log lines go to `X-Plane/Log.txt` with the prefix `PauseAtToD:`.
