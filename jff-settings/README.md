# jff settings

A GTK4 / libadwaita app that edits jff's `config.kdl`. It lives outside `src/` with its
own workspace, so the compositor gains no GTK dependency and `cargo build --release` at the
repo root is unchanged.

```
cd jff-settings
cargo run --release -p jff-settings
```

Needs the development packages of GTK 4.10 or newer and libadwaita 1.4 or newer.

## How it works

- Edits the one file jff reads: `$JFF_CONFIG`, then `$XDG_CONFIG_HOME/jff/config.kdl`,
  then `~/.config/jff/config.kdl`. If there is none, the first change creates it from jff's
  built-in defaults.
- Comments and formatting of everything it doesn't touch are kept.
- Each change is saved half a second after the last one. The save is checked with
  `jff check-config FILE`; if jff refuses it, the old file is put back and a message says why.
  jff watches the file, so a good save applies at once.
- The first save of a session copies an existing file to `config.kdl.bak`.
- If `jff` is not on `PATH`, the check is skipped.
- Settings that only exist by being written (blur, the workspace wall, the border) are
  switched on and off from their first row, and the rows below are greyed out while they are off.
  That way editing a detail never turns a feature on by accident.

## Pages

| Page         | Settings                                                                                                                          |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------- |
| Appearance   | gaps, radius, popup radius, server-side decorations, border (width, colors, animation)                                            |
| Keyboard     | layout, variant, options, rules, model, repeat delay and rate, Num Lock                                                           |
| Windows      | workspaces (count, per monitor), maximize new windows, drag modifier, global hotkeys, window animations                           |
| Effects      | blur (with its lists), on-screen display, window switcher, overview, workspace wall, hot corner                                   |
| Monitors     | one entry per output: off, position, mode, variable refresh rate, HDR (SDR white, peak, tone mapping, content brightness), ICC profile, game hint, color management |
| Window rules | add, edit, reorder and remove rules: match (app id, title, tag, dialog, restored) and actions                                     |
| Key bindings | add, edit and remove bindings; every action jff knows, with its arguments                                                         |
| Startup      | `autostart` and `sh-autostart` lines                                                                                              |
| System       | screenshots, logging, color management, tearing                                                                                   |

## Limits

- A window rule with several `match` lines shows the first; the others are kept as they are.
- A key combination can only use letters, digits and `+ _ - .` (symbols by name: `comma`).
- If jff refuses a change, the file goes back, but the value stays in the window and later
  saves fail the same way until you fix it. The message says what jff did not like.

## Adding a setting

Add a `Field` to `crates/jff-settings/src/pages.rs`. Use only keys jff knows: an unknown key
inside a known block is an error for jff.
