<!--
Adding a wallpaper to the compatibility list? Only site/wallpapers.json and a
screenshot in site/shots/ are needed — run `python3 site/validate.py` and
delete the rest of this template.
-->

## What this changes

<!-- What it does, and why it is worth doing. Link the issue it closes. -->

## How it was tested

<!--
Which wallpapers/screens/compositor it ran against, not just that it compiles.
Rendering changes: say what you compared the output to.
-->

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` are clean
- [ ] `cargo test --workspace` passes
- [ ] Touched a render path? Checked it against a real wallpaper, not only the tests
