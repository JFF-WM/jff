# alpha-test-client

Manual test for `wp_alpha_modifier_v1` in jff. Rendering can't be unit-tested, so this opens a
window with a multiplier of your choosing and you look at it.

```
cargo run --manifest-path tools/alpha-test-client/Cargo.toml -- 50
cargo run --manifest-path tools/alpha-test-client/Cargo.toml -- 50 100   # child stays opaque
```

The window is opaque red | blue halves with a bottom strip that is already 50% transparent in
its own pixels. Put something bright behind it (a wallpaper, another window).

Run each of the 3 levels (`0`, `50`, `100`) under each of the 4 configs: plain, `radius 16`,
`blur { all-windows }`, and `radius 16` + `blur { all-windows }` + a `border { }` block.

| Level | Expect |
|---|---|
| 100 | Identical to a client that never sets a multiplier: opaque halves, full blur, full border. |
| 50  | Halves are see-through (what is behind shows). Bottom strip is fainter than the halves (the multiplier stacks on pixel alpha). Rounded corners are still see-through, not opaque. Blur is half strength. Border is half strength. |
| 0   | Window, blur and border are all gone: nothing but what is behind. |

Also check:

- `50 100`: the green square is fully opaque while the parent is faded (children are unaffected).
- Fullscreen at 50 on the TTY: the desktop behind shows through, and with debug logging the
  monitor must say the primary plane shows `the composited frame`, not `the client's buffer
  (direct scanout)`. At 100 it goes back to direct scanout.
- Overview: a 50% window's thumbnail is 50% too, and its blur follows.
- Screenshot of the window (`capture_window`): translucent where the window is.
- Closing the window: the fade-out starts from the faded look, with no flash to opaque.
- Run it twice at different levels: the multiplier is per surface, not global.
