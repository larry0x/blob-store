# The routine recipes every repository shares: format, lint, test, and bump,
# for every language any of the repositories uses. It comes from the tools
# repository, https://github.com/larry0x/tools, and `just sync-tools`
# copies it in. `nix flake check` fails when the copy differs from the version
# flake.lock pins, so change it there, not in the copy.
#
# A repository with no recipes of its own takes this file as its justfile. A
# repository with recipes of its own takes it as base.just, and its justfile
# opens with `import 'base.just'`. The two settings below let that justfile
# replace any recipe or variable defined here, by defining one of the same
# name.
set allow-duplicate-recipes
set allow-duplicate-variables

# Each language's recipes do nothing, and succeed, in a repository without
# that language, so `ci`, `fmt`, `fmt-check`, `lint`, and `test` run unchanged
# in every repository. A recipe knows by a marker file, such as Cargo.toml, or
# by its list of sources being empty.
#
# Every tool comes from the Nix dev shell, so a clean checkout needs only nix
# and just. A recipe first checks whether it has work to do, and only then
# enters the dev shell, at the line `{{ enter }}`: unless it already runs in
# one, the script runs itself again under `nix develop`. CI runs
# `nix develop --command just <recipe>`, so there the line does nothing.
set script-interpreter := ['bash', '-euo', 'pipefail']

# The command that runs a tool in the dev shell. `enter` is built from it,
# and a repository's own recipes can prefix their lines with it.
dev := "nix develop --command"

enter := '[ -n "${IN_NIX_SHELL:-}" ] || exec ' + dev + ' bash -euo pipefail "$0" "$@"'

# The sources of one language: tracked files plus new ones git does not
# ignore, minus tracked files deleted from disk but not yet staged. So a new
# file is covered before its first `git add`, a git-ignored scratch copy is
# not, and a pending deletion does not hand a tool a path that is gone. A path
# may not contain a space. Each list is a shell command substitution, so it is
# computed when a recipe runs, not when just starts.
ls-files := "git ls-files --cached --others --exclude-standard --deduplicate"
on-disk := "while IFS= read -r f; do if [ -f \"$f\" ]; then echo \"$f\"; fi; done"
nix := "$(" + ls-files + " '*.nix' | " + on-disk + ")"
toml := "$(" + ls-files + " '*.toml' | " + on-disk + ")"
yaml := "$(" + ls-files + " '*.yaml' '*.yml' | " + on-disk + ")"
markdown := "$(" + ls-files + " '*.md' | " + on-disk + ")"
tex := "$(" + ls-files + " '*.tex' | " + on-disk + ")"

# List available recipes
[default]
list:
  @just --list --unsorted

# ------------------------------------ All -------------------------------------

# The pre-commit hook runs `just ci` before every commit, and aborts the
# commit when it fails. It is every check CI runs. It installs the JavaScript
# dependencies first, as CI does, because svelte-check and vitest read
# node_modules: so a clean checkout passes, and a stale node_modules cannot
# hide a package-lock.json change.
#
# Run every check: formatting, linters, and tests
[group('all')]
ci: install-js-deps fmt-check lint test

# Format every source
[group('all')]
fmt: fmt-nix fmt-rust fmt-python fmt-latex fmt-js fmt-toml fmt-yaml fmt-markdown

# markdownlint-cli2 is both the Markdown formatter and its linter, so the
# Markdown check runs in `lint` only.
#
# Report formatting drift in every source, changing nothing
[group('all')]
fmt-check: fmt-check-nix fmt-check-rust fmt-check-python fmt-check-latex fmt-check-js fmt-check-toml fmt-check-yaml

# Lint every source
[group('all')]
lint: lint-nix lint-rust lint-lean lint-python lint-latex lint-js lint-toml lint-yaml lint-markdown

# Run every test suite
[group('all')]
test: test-nix test-rust test-lean test-python test-js

# ----------------------------------- Tools ------------------------------------

# Every input in flake.lock moves to its latest version at once: nixpkgs, and
# with it every tool in the dev shell; the tools repository; and any other
# input, such as nix-darwin or sops-nix. nix itself is not a tool out of the
# dev shell, so the recipe does not enter one. sync-tools runs after the
# body, so the new lock and the new copies land in the same commit.
#
# The last lines read the Lean version from the nixpkgs that flake.lock now
# pins, and write it to lean-toolchain for the editor extension.
#
# Bump every flake input, and copy in the shared files at the new version
[group('tools')]
[script]
bump-tools: && sync-tools
  nix flake update
  if [ -f lean-toolchain ]; then
    echo "leanprover/lean4:v$(nix eval --raw --inputs-from . nixpkgs#lean4.version)" > lean-toolchain
  fi

# A repository that takes files from the tools repository has the flake app
# sync-tools, which copies them in, and pins the repository's calls to the
# reusable workflows. The tools repository itself has no such app, so there
# the recipe does nothing. A flake without an `apps` output counts as having
# no such app; any other error in evaluating the flake stops the recipe, and
# shows. getFlake reads the working tree, as `nix run .#…` does, which takes
# --impure.
#
# Copy in the shared files, and pin the workflow calls, at the version flake.lock pins
[group('tools')]
[script]
sync-tools:
  has_sync=$(nix eval --impure --expr 'let flake = builtins.getFlake (toString ./.); in builtins.any (system: system ? "sync-tools") (builtins.attrValues (flake.apps or { }))')
  [ "$has_sync" = true ] || exit 0
  nix run .#sync-tools

# ------------------------------------ Nix -------------------------------------

# Format the Nix files
[group('nix')]
[script]
fmt-nix:
  files={{ nix }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  nixfmt $files

# Report Nix formatting drift, changing nothing
[group('nix')]
[script]
fmt-check-nix:
  files={{ nix }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  nixfmt --check $files

# statix takes one path per run and does not honor every ignore source, such
# as .git/info/exclude, so it runs in a loop.
#
# nixf-tidy runs the checks of nixf, the library behind nixd, the language
# server that editors show, so whatever the editor flags in a Nix file fails
# here too. --variable-lookup adds the check for undefined variables, which
# the editor runs as well. nixf-tidy prints a JSON list on stdout, empty for a
# clean file, and exits 0 either way, a parse error included, so the loop
# fails on any non-empty list. jq prints each finding as
# file:line:column: message (code), filling the {} slots in the message from
# its args.
#
# Lint the Nix files
[group('nix')]
[script]
lint-nix:
  files={{ nix }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  s=0
  for f in $files; do
    statix check "$f" || s=1
  done
  deadnix --fail $files || s=1
  for f in $files; do
    out=$(nixf-tidy --variable-lookup < "$f") || s=1
    if [ "$out" != "[]" ]; then
      s=1
      printf '%s\n' "$out" | jq -r --arg f "$f" \
        '.[] | "\($f):\(.range.lCur.line + 1):\(.range.lCur.column + 1): \(reduce .args[] as $a (.message; sub("\\{\\}"; $a))) (\(.sname))"'
    fi
  done
  exit $s

# nix itself is not a tool out of the dev shell, so this recipe does not
# enter one. It evaluates every output for every system, and runs the flake's
# checks, among them the one that the files copied from the tools
# repository are unchanged. Development happens on a Mac, so this is the
# cheap way to catch a mistake specific to Linux.
#
# Test the Nix files: evaluate every output for every system, and run the checks
[group('nix')]
[script]
test-nix:
  [ -f flake.nix ] || exit 0
  nix flake check --all-systems

# ------------------------------------ Rust ------------------------------------

# Bump the Rust dependencies in Cargo.lock
[group('rust')]
[script]
bump-rust-deps:
  [ -f Cargo.toml ] || exit 0
  {{ enter }}
  cargo update

# Format the Rust sources
[group('rust')]
[script]
fmt-rust:
  [ -f Cargo.toml ] || exit 0
  {{ enter }}
  cargo fmt --all

# Report Rust formatting drift, changing nothing
[group('rust')]
[script]
fmt-check-rust:
  [ -f Cargo.toml ] || exit 0
  {{ enter }}
  cargo fmt --all --check

# Cargo features are additive, so the two ends of the range cover the code:
# no feature at all, and every feature at once. Lint checks both ends. A test
# that builds with fewer features also builds with all of them, so the tests
# run with every feature only. For a crate without features the two clippy
# runs are the same, and the second one is answered from the cache.
#
# Lint the Rust sources
[group('rust')]
[script]
lint-rust:
  [ -f Cargo.toml ] || exit 0
  {{ enter }}
  cargo clippy --workspace --all-targets --no-default-features -- -D warnings
  cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the Rust tests, doc-tests included
[group('rust')]
[script]
test-rust:
  [ -f Cargo.toml ] || exit 0
  {{ enter }}
  RUST_BACKTRACE=1 cargo test --workspace --all-features -- --nocapture

# ------------------------------------ Lean ------------------------------------

# Lean's linters run inside the compiler and report warnings, so a build that
# fails on any warning is the lint. `lake lint --builtin-only` would run the
# same linters, but it reads only a library's `roots` and ignores `globs`.
# Lean has no official formatter, so there is no fmt-lean.
#
# Lint the Lean files: build them all, and fail on any warning
[group('lean')]
[script]
lint-lean:
  [ -f lakefile.toml ] || [ -f lakefile.lean ] || exit 0
  {{ enter }}
  lake build --wfail

# Run the `#guard` and `#guard_msgs` checks in the Lean files
[group('lean')]
[script]
test-lean:
  [ -f lakefile.toml ] || [ -f lakefile.lean ] || exit 0
  {{ enter }}
  lake test

# ----------------------------------- Python -----------------------------------

# ruff and mypy read their settings from pyproject.toml.
#
# Bump the Python dependencies in uv.lock
[group('python')]
[script]
bump-python-deps:
  [ -f uv.lock ] || exit 0
  {{ enter }}
  uv lock --upgrade

# Format the Python sources
[group('python')]
[script]
fmt-python:
  [ -f pyproject.toml ] || exit 0
  {{ enter }}
  ruff check --select I --fix .
  ruff format .

# Report Python formatting drift, changing nothing
[group('python')]
[script]
fmt-check-python:
  [ -f pyproject.toml ] || exit 0
  {{ enter }}
  ruff check --select I .
  ruff format --check .

# Lint the Python sources
[group('python')]
[script]
lint-python:
  [ -f pyproject.toml ] || exit 0
  {{ enter }}
  ruff check .
  mypy

# Run the Python tests
[group('python')]
[script]
test-python:
  [ -f pyproject.toml ] || exit 0
  {{ enter }}
  python3 -m unittest discover

# ----------------------------------- LaTeX ------------------------------------

# latexindent reads its settings from .latexindent.yaml, passed with -l. -s
# silences it, and -g /dev/null drops its log file. -o rewrites a file in
# place and leaves no backup. chktex exits non-zero on any warning, so a
# warning fails the lint.
#
# Format the LaTeX files
[group('latex')]
[script]
fmt-latex:
  files={{ tex }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  for f in $files; do
    latexindent -s -g /dev/null -l=.latexindent.yaml -o="$f" "$f"
  done

# Report LaTeX formatting drift as a diff, changing nothing
[group('latex')]
[script]
fmt-check-latex:
  files={{ tex }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  s=0
  for f in $files; do
    latexindent -s -g /dev/null -l=.latexindent.yaml -kv "$f" || s=1
  done
  exit $s

# Lint the LaTeX files
[group('latex')]
[script]
lint-latex:
  files={{ tex }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  s=0
  for f in $files; do
    chktex -q "$f" || s=1
  done
  exit $s

# --------------------------------- JavaScript ---------------------------------

# Biome formats and lints JavaScript, TypeScript, CSS, JSON, and Svelte,
# configured by biome.json; npm runs the package's own scripts. A repository
# can have either one without the other: Biome alone checks loose files, and
# a package.json without biome.json still has dependencies to install.
#
# Install the JavaScript dependencies per package-lock.json
[group('js')]
[script]
install-js-deps:
  [ -f package.json ] || exit 0
  {{ enter }}
  npm ci

# --peer keeps each package within the versions its peers accept.
#
# Bump every package.json dependency past its semver major, as peers allow
[group('js')]
[script]
bump-js-deps:
  [ -f package.json ] || exit 0
  {{ enter }}
  npm-check-updates -u --peer
  npm install

# Format the JavaScript, TypeScript, CSS, JSON, and Svelte sources
[group('js')]
[script]
fmt-js:
  [ -f biome.json ] || exit 0
  {{ enter }}
  biome format --write .

# Report JavaScript formatting drift, changing nothing
[group('js')]
[script]
fmt-check-js:
  [ -f biome.json ] || exit 0
  {{ enter }}
  biome format .

# A warning fails the lint, as clippy's `-D warnings` does for Rust. The
# package's `check` script, where there is one, runs the type checker, such
# as svelte-check.
#
# Lint the JavaScript sources
[group('js')]
[script]
lint-js:
  [ -f biome.json ] || [ -f package.json ] || exit 0
  {{ enter }}
  if [ -f biome.json ]; then biome lint --error-on-warnings .; fi
  if [ -f package.json ]; then npm run --if-present check; fi

# Run the JavaScript tests
[group('js')]
[script]
test-js:
  [ -f package.json ] || exit 0
  {{ enter }}
  npm run --if-present test

# ------------------------------------ TOML ------------------------------------

# taplo reads its formatting options from taplo.toml.
#
# Format the TOML files
[group('toml')]
[script]
fmt-toml:
  files={{ toml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  taplo fmt $files

# Report TOML formatting drift, changing nothing
[group('toml')]
[script]
fmt-check-toml:
  files={{ toml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  taplo fmt --check --diff $files

# Lint the TOML files
[group('toml')]
[script]
lint-toml:
  files={{ toml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  taplo lint $files

# ------------------------------------ YAML ------------------------------------

# yamlfmt reads the files to format from .yamlfmt.yaml, and yamllint reads
# its rules from .yamllint.yaml. actionlint checks what yamllint cannot in
# the CI workflows: the workflow syntax, expressions, and the shell scripts in
# `run:` steps.
#
# Format the YAML files
[group('yaml')]
[script]
fmt-yaml:
  files={{ yaml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  yamlfmt

# Report YAML formatting drift, changing nothing
[group('yaml')]
[script]
fmt-check-yaml:
  files={{ yaml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  yamlfmt -lint

# actionlint finds the workflows in .github/workflows/ by itself, but not
# those in .forgejo/workflows/, which it is handed by path. It checks them
# by GitHub's rules, so two findings are ignored there: Forgejo's runners
# carry the label `native`, which actionlint does not know, and Forgejo
# needs `runs-on` on a job that calls a reusable workflow, which GitHub
# forbids.

# Lint the YAML files
[group('yaml')]
[script]
lint-yaml:
  files={{ yaml }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  yamllint --strict .
  if [ -d .github/workflows ]; then actionlint; fi
  if [ -d .forgejo/workflows ]; then
    actionlint \
      -ignore 'label "native" is unknown' \
      -ignore '"runs-on" is not available' \
      .forgejo/workflows/*.y*ml
  fi

# ---------------------------------- Markdown ----------------------------------

# markdownlint-cli2 reads the files to check, and the rules, from
# .markdownlint-cli2.yaml. --fix repairs what it can, and reports the rest.
#
# Fix the Markdown lint errors that can be fixed automatically
[group('markdown')]
[script]
fmt-markdown:
  files={{ markdown }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  markdownlint-cli2 --fix

# Lint the Markdown files
[group('markdown')]
[script]
lint-markdown:
  files={{ markdown }}
  [ -n "$files" ] || exit 0
  {{ enter }}
  markdownlint-cli2
