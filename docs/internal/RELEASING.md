# Releasing unwreck 0.1.0

Repo: https://github.com/ayushdoesdev/unwreck  
The Cargo workspace is `code/`. Crates must be published in dependency order.

## 1. Create the GitHub repo

```sh
cd /path/to/unwreck
git init
git add -A
git commit -m "unwreck 0.1.0"
gh repo create ayushdoesdev/unwreck --public --source . --remote origin
git push -u origin HEAD:main
```

## 2. Publish crates (crates.io login is yours)

From `code/`, after `cargo login`:

```sh
cargo publish -p unwreck-core
# wait ~30s for the index
cargo publish -p unwreck-store
cargo publish -p unwreck-scan
cargo publish -p unwreck-watch
cargo publish -p unwreck-hooks
cargo publish -p unwreck-restore
cargo publish -p unwreck-tui
cargo publish -p unwreck
```

## 3. Tag the GitHub Release

```sh
git tag v0.1.0
git push origin v0.1.0
```

`.github/workflows/release.yml` builds:

- `x86_64-unknown-linux-musl`
- `x86_64-apple-darwin`, `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

(`aarch64-unknown-linux-musl` is deferred to 0.1.1.)

## 4. Verify

```sh
cargo install unwreck
unwreck --version   # 0.1.0
```
