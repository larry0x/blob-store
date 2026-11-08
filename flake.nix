{
  description = "Content-addressed file store with a database catalog";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # The shared configuration files, justfile recipes, and tools. `just
    # sync-tools` copies the files in at the version flake.lock pins.
    tools = {
      url = "github:larry0x/tools";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      tools,
    }:
    let
      # nixpkgs is the single source of truth for the Rust toolchain: the
      # devShell and CI both take it from here. CI runs on x86_64-linux;
      # development happens on an Apple Silicon Mac.
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
      ];

      forEachSystem = f: nixpkgs.lib.genAttrs systems f;

      # The files this repository takes from the tools repository. It has no
      # recipes of its own, so the base justfile is its justfile.
      toolsFiles = [
        ".editorconfig"
        ".gitignore"
        ".markdownlint-cli2.yaml"
        ".yamlfmt.yaml"
        ".yamllint.yaml"
        "clippy.toml"
        "justfile"
        "rustfmt.toml"
        "taplo.toml"
      ];
    in
    {
      # Fails while a file taken from the tools repository differs from the
      # pinned version, or a workflow calls the tools repository at another
      # commit.
      checks = forEachSystem (system: {
        tools = tools.lib.check {
          pkgs = nixpkgs.legacyPackages.${system};
          src = self;
          files = toolsFiles;
        };
      });

      # `just sync-tools` runs this, to copy the files in and pin the calls.
      apps = forEachSystem (system: {
        sync-tools = tools.lib.sync {
          pkgs = nixpkgs.legacyPackages.${system};
          files = toolsFiles;
        };
      });

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
            # just, and the formatters and linters for Nix, TOML, YAML, and
            # Markdown, come from the tools repository, at the versions of
            # the nixpkgs pinned here.
            packages = tools.lib.packages pkgs ++ [
              # The Rust toolchain: four packages built from one rustc
              # release, so their versions cannot skew; bump with
              # `just bump-tools`. cargo does not pull in the compiler by
              # itself, so rustc must be listed where this is built.
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
            ];
          };
        }
      );
    };
}
