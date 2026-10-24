{
  description = "Content-addressed file store with a database catalog";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      # nixpkgs is the single source of truth for the Rust toolchain: the
      # devShell and CI both take it from here. CI runs on x86_64-linux;
      # development happens on an Apple Silicon Mac.
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
      ];

      forEachSystem = f: nixpkgs.lib.genAttrs systems f;
    in
    {
      # A library has no package of its own: an app builds it from source as
      # a cargo dependency.

      # The development shell: every tool the just recipes wrap.
      devShells = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              # The task runner. Every recipe wraps its commands in
              # `nix develop`, and CI runs the same recipes, so this is the
              # entry point on both sides.
              pkgs.just

              # The Rust toolchain: four packages built from one rustc
              # release, so their versions cannot skew; bump with
              # `just bump-toolchain`. cargo does not pull in the compiler by
              # itself, so rustc must be listed where this is built.
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt

              # Formatter and linter for TOML files, configured by taplo.toml.
              pkgs.taplo

              # Formatter and linters for the Nix files.
              pkgs.nixfmt
              pkgs.statix
              pkgs.deadnix
            ];
          };
        }
      );
    };
}
