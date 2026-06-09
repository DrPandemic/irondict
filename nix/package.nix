{ lib
, rustPlatform
, pkg-config
, makeWrapper
, fontconfig
, freetype
, libGL
, libxkbcommon
, mesa
, wayland
, libx11
, libxcursor
, libxi
, libxrandr
, libxcb
}:

let
  cargoToml = lib.importTOML ../Cargo.toml;

  # Libraries Slint's winit + femtovg renderer (and rfd's portal dialogs)
  # dlopen at runtime. They must be on the loader path of the GUI binary, so we
  # also wrap with LD_LIBRARY_PATH below.
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
rustPlatform.buildRustPackage {
  pname = "irondict";
  version = cargoToml.workspace.package.version;

  src = lib.cleanSourceWith {
    src = ../.;
    # Keep the build hermetic and small: drop build artifacts and the AUR
    # packaging tree (which carries a vendored copy of the repo).
    filter = path: type:
      let rel = lib.removePrefix (toString ../. + "/") (toString path);
      in !(lib.hasPrefix "target" rel
        || lib.hasPrefix "packaging/aur" rel
        || rel == "result");
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = runtimeLibs;

  # Build only the front-end binaries; `irondict-core` is pulled in as a dep.
  cargoBuildFlags = [ "-p" "irondict-gui" "-p" "irondict-cli" ];
  cargoTestFlags = [ "--workspace" ];

  postInstall = ''
    # Bundled public-domain GCIDE dictionary. The binary resolves
    # `<exe-dir>/../share/irondict/gcide`, which lands here under $out.
    install -Dm644 -t $out/share/irondict/gcide \
      crates/core/assets/gcide/dictd_www.dict.org_gcide.ifo \
      crates/core/assets/gcide/dictd_www.dict.org_gcide.idx \
      crates/core/assets/gcide/dictd_www.dict.org_gcide.dict.dz

    install -Dm644 packaging/irondict.desktop \
      $out/share/applications/irondict.desktop

    install -Dm644 crates/gui/assets/icons/irondict.svg \
      $out/share/icons/hicolor/scalable/apps/irondict.svg
    for s in 16 32 48 64 128 256 512; do
      install -Dm644 crates/gui/assets/icons/hicolor/''${s}x''${s}/apps/irondict.png \
        $out/share/icons/hicolor/''${s}x''${s}/apps/irondict.png
    done

    install -Dm644 -t $out/share/licenses/irondict LICENSE docs/gcide.md
  '';

  postFixup = ''
    # Slint's femtovg renderer needs an EGL/OpenGL display. The bundled libglvnd
    # has no GPU vendor of its own, so point it at our own Mesa (absolute paths
    # in its vendor JSON) — this makes the GUI self-contained and run on
    # non-NixOS hosts too, with llvmpipe as a software fallback. `--set-default`
    # lets a system driver take over when the env var is already set.
    wrapProgram $out/bin/irondict-gui \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs} \
      --set-default __EGL_VENDOR_LIBRARY_DIRS ${mesa}/share/glvnd/egl_vendor.d
  '';

  meta = {
    description = "Fast local multi-dictionary lookup with fuzzy and full-text search — CLI and GUI";
    homepage = "https://github.com/DrPandemic/irondict";
    license = lib.licenses.gpl3Plus;
    mainProgram = "irondict-gui";
    platforms = lib.platforms.linux;
  };
}
