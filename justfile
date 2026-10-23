set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# ---------------------------------------------------------------------------
# Cross-platform recipes (only shell-agnostic tooling — cargo, npm).
# ---------------------------------------------------------------------------

# Build the plugin binary in debug mode (plus UI if present).
build: build-ui
    cargo build

# Build for release (what the GitHub Actions workflow ships).
release: build-ui
    cargo build --release

# Run unit tests.
test:
    cargo test

# Launch the local REPL that simulates Tabularis JSON-RPC calls over stdio.
repl:
    cargo run --bin test_plugin

# Run clippy on the workspace.
lint:
    cargo clippy --all-targets -- -D warnings

# Format the codebase.
fmt:
    cargo fmt --all

# ---------------------------------------------------------------------------
# Platform-specific recipes (file operations + plugin-dir conventions).
# ---------------------------------------------------------------------------

# Build the UI extension if present (no-op otherwise).
[unix]
build-ui:
    @if [ -f ui/package.json ]; then \
        echo "Building UI extension..."; \
        (cd ui && npm install --no-audit --no-fund && npm run build); \
    fi

[windows]
build-ui:
    if (Test-Path ui/package.json) {
        Write-Host "Building UI extension..."
        Push-Location ui
        try {
            npm install --no-audit --no-fund
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            npm run build
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        } finally {
            Pop-Location
        }
    }

# Build + copy binary, manifest, and (if present) UI bundle and locales into Tabularis's plugin folder.
# The binary is looked up in Cargo's real target directory (from `cargo metadata`),
# so CARGO_TARGET_DIR and `build.target-dir` in Cargo config both work.
[linux]
dev-install: build
    mkdir -p ~/.local/share/tabularis/plugins/drivers/cockroachdb
    target_dir="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"; \
    cp "$target_dir/debug/cockroachdb-plugin" ~/.local/share/tabularis/plugins/drivers/cockroachdb/
    cp .tabularium ~/.local/share/tabularis/plugins/drivers/cockroachdb/
    @if [ -d locales ]; then \
        mkdir -p ~/.local/share/tabularis/plugins/drivers/cockroachdb/locales; \
        cp -r locales/* ~/.local/share/tabularis/plugins/drivers/cockroachdb/locales/; \
    fi
    @if [ -f ui/dist/index.js ]; then \
        mkdir -p ~/.local/share/tabularis/plugins/drivers/cockroachdb/ui/dist; \
        cp ui/dist/index.js ~/.local/share/tabularis/plugins/drivers/cockroachdb/ui/dist/; \
    fi
    @echo "Installed to ~/.local/share/tabularis/plugins/drivers/cockroachdb"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[macos]
dev-install: build
    mkdir -p "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb"
    target_dir="$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"; \
    cp "$target_dir/debug/cockroachdb-plugin" "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/"
    cp .tabularium "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/"
    @if [ -d locales ]; then \
        mkdir -p "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/locales"; \
        cp -r locales/* "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/locales/"; \
    fi
    @if [ -f ui/dist/index.js ]; then \
        mkdir -p "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/ui/dist"; \
        cp ui/dist/index.js "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb/ui/dist/"; \
    fi
    @echo "Installed to ~/Library/Application Support/tabularis/plugins/drivers/cockroachdb"
    @echo "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

[windows]
dev-install: build
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\cockroachdb"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item (Join-Path (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory "debug\cockroachdb-plugin.exe") (Join-Path $env:APPDATA "tabularis\plugins\drivers\cockroachdb")
    Copy-Item ".tabularium" $dest
    if (Test-Path "locales") {
        New-Item -ItemType Directory -Force -Path (Join-Path $dest "locales") | Out-Null
        Copy-Item -Recurse -Force "locales\*" (Join-Path $dest "locales")
    }
    if (Test-Path "ui\dist\index.js") {
        New-Item -ItemType Directory -Force -Path (Join-Path $dest "ui\dist") | Out-Null
        Copy-Item "ui\dist\index.js" (Join-Path $dest "ui\dist")
    }
    Write-Host "Installed to $dest"
    Write-Host "Restart Tabularis (or toggle the plugin in Settings) to pick up changes."

# Remove the kind-scoped development install. Manage old flat copies in Settings.
[linux]
uninstall:
    rm -rf ~/.local/share/tabularis/plugins/drivers/cockroachdb

[macos]
uninstall:
    rm -rf "$HOME/Library/Application Support/tabularis/plugins/drivers/cockroachdb"

[windows]
uninstall:
    $dest = Join-Path $env:APPDATA "tabularis\plugins\drivers\cockroachdb"
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
