# Agent detection rules

These rule files tell what an agent CLI without lifecycle hooks is doing from what it
draws in the terminal. They are copied unmodified from
[herdrdev/herdr](https://github.com/herdrdev/herdr) (`distribution/agent-detection/`,
commit `bce28752adb4eea0788013b9e2b0793d82737dfe`) and are licensed under the Apache
License 2.0; see `LICENSE-APACHE`.

`src/screen_detection.rs` evaluates them with a matcher modeled on herdr's
`src/detect/manifest.rs`.
