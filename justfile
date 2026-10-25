# Every tool comes from the nix devShell, so a clean checkout needs only nix.
dev := "nix develop --command"

# The Nix sources: tracked files plus new ones git does not ignore, minus
# tracked files deleted from disk but not yet staged. So a new file is
# covered before its first `git add`, a git-ignored scratch copy is not, and
# a pending deletion does not hand a tool a path that is gone. A path may not
# contain a space.
nix := "$(git ls-files --cached --others --exclude-standard --deduplicate '*.nix' | while IFS= read -r f; do [ -f \"$f\" ] && echo \"$f\"; done)"

# List available recipes
default:
  @just --list

# ------------------------------------ All -------------------------------------

# Format every source: Rust, TOML, and Nix
[group('all')]
fmt: fmt-rust fmt-nix

# Report formatting drift in every source, changing nothing
[group('all')]
fmt-check: fmt-check-rust fmt-check-nix

# Lint every source
[group('all')]
lint: lint-rust lint-nix

# Run every test suite
[group('all')]
test: test-rust

# ------------------------------------ Rust ------------------------------------

# Format the Rust crate (TOML + Rust)
[group('rust')]
fmt-rust:
  {{ dev }} taplo fmt
  {{ dev }} cargo fmt --all

# Report Rust formatting drift, changing nothing
[group('rust')]
fmt-check-rust:
  {{ dev }} taplo fmt --check --diff
  {{ dev }} cargo fmt --all --check

# Every feature is additive, so the two ends of the range cover the code: no
# feature at all, and every feature at once. Lint checks both ends. The tests
# need a hasher, so they run with every feature only: a test that builds with
# fewer features also builds with all of them.

# Lint the Rust crate
[group('rust')]
lint-rust:
  {{ dev }} taplo lint
  {{ dev }} cargo clippy --no-default-features --all-targets -- -D warnings
  {{ dev }} cargo clippy --all-features --all-targets -- -D warnings

# Run the Rust tests
[group('rust')]
test-rust:
  RUST_BACKTRACE=1 {{ dev }} cargo test --all-features -- --nocapture

# ------------------------------------ Nix -------------------------------------

# Format the Nix files
[group('nix')]
fmt-nix:
  {{ dev }} nixfmt {{ nix }}

# Report Nix formatting drift, changing nothing
[group('nix')]
fmt-check-nix:
  {{ dev }} nixfmt --check {{ nix }}

# The last line is nix itself, not a tool out of the shell, which is why it
# carries no prefix. Development happens on a Mac, so x86_64-linux is never
# evaluated there. This evaluates every output for both systems and builds
# none of them, which is the cheap way to catch a Linux-only mistake.

# statix takes one path per run and does not honor every ignore source, such
# as .git/info/exclude, so lint-nix loops over the list.

# Lint the Nix files
[group('nix')]
lint-nix:
  {{ dev }} sh -c 's=0; for f in "$@"; do statix check "$f" || s=1; done; exit $s' _ {{ nix }}
  {{ dev }} deadnix --fail {{ nix }}
  nix flake check --all-systems

# ------------------------------------ Bump ------------------------------------

# Bump the pinned toolchain (nixpkgs)
[group('bump')]
bump-toolchain:
  nix flake update nixpkgs

# Bump the versions of cargo dependencies
[group('bump')]
bump-deps:
  {{ dev }} cargo update
