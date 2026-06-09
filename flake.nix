{
  description = "IronDict — fast local multi-dictionary lookup with fuzzy and full-text search (CLI + GUI)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      # Linux-only: the GUI uses Slint (winit + femtovg) which pulls in
      # Wayland/X11/OpenGL. Add more here if you want Darwin etc.
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        irondict = pkgs.callPackage ./nix/package.nix { };
        default = irondict;
      });

      apps = forAllSystems (pkgs:
        let irondict = self.packages.${pkgs.system}.irondict; in
        {
          default = self.apps.${pkgs.system}.gui;
          gui = {
            type = "app";
            program = "${irondict}/bin/irondict-gui";
          };
          cli = {
            type = "app";
            program = "${irondict}/bin/irondict-cli";
          };
        });

      devShells = forAllSystems (pkgs: {
        default = pkgs.callPackage ./nix/shell.nix { };
      });

      formatter = forAllSystems (pkgs: pkgs.nixpkgs-fmt);
    };
}
