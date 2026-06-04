# irondict

A dictionary app with both a CLI and a GUI front-end. Written in Rust.

## Notes

- Keep CLI and GUI logic separate from the core dictionary lookup, so both
  front-ends share the same backend.
