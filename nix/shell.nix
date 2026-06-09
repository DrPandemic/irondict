{ mkShell
, lib
, rustc
, cargo
, rustfmt
, clippy
, rust-analyzer
, pkg-config
, fontconfig
, freetype
, libGL
, libxkbcommon
, wayland
, libx11
, libxcursor
, libxi
, libxrandr
, libxcb
}:

let
  runtimeLibs = [
    fontconfig
    freetype
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
    libxcb
  ];
in
mkShell {
  nativeBuildInputs = [
    rustc
    cargo
    rustfmt
    clippy
    rust-analyzer
    pkg-config
  ];
  buildInputs = runtimeLibs;

  # `cargo run -p irondict-gui` runs the unwrapped binary, so the renderer's
  # dlopen'd libs need to be reachable here too.
  LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
}
