# irondict

A dictionary app with both a CLI and a GUI front-end. Written in Rust.

## Notes

- Keep CLI and GUI logic separate from the core dictionary lookup, so both
  front-ends share the same backend.

## Crate layout

```
irondict/
├── Cargo.toml            # [workspace] members
└── crates/
    ├── core/   (irondict-core)  # library: model, StarDict loader, manager, search
    ├── cli/    (irondict-cli)   # binary: clap-based CLI front-end
    └── gui/    (irondict-gui)   # binary: Slint front-end (ui.slint + main.rs)
```