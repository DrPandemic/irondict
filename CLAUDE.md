# irondict

A dictionary app with both a CLI and a GUI front-end. Written in Rust.

A single `irondict` binary serves both front-ends: subcommands run the CLI,
`--gui` launches the graphical interface.

## Notes

- Keep CLI and GUI logic separate from the core dictionary lookup, so both
  front-ends share the same backend.

## Crate layout

```
irondict/
├── Cargo.toml            # [workspace] members
└── crates/
    ├── core/  (irondict-core)  # library: model, StarDict loader, manager, search
    └── app/   (irondict)       # binary: clap CLI (main.rs) + Slint GUI (gui.rs, --gui)
```