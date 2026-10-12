# Build

Use an Apple Silicon Mac, Xcode Command Line Tools (`xcode-select --install`), and a current stable Rust toolchain. The package declares Rust 1.85 as its minimum; the development toolchain used for recent checks was Rust 1.99. Minimum-version compatibility is not independently verified.

From the repository root:

```sh
cargo build --release --locked --manifest-path rust/Cargo.toml
cargo test --locked --manifest-path rust/Cargo.toml
cargo clippy --locked --manifest-path rust/Cargo.toml --all-targets -- -D warnings
```

Inspect an independently supplied ELF:

```sh
rust/target/release/native-ps5 inspect /absolute/path/to/test.elf
```

This is a development CLI, not a game launcher. `verify-gpu`, `verify-arithmetic`, `verify-native` and `verify-payload` are research verification commands; they may open windows, use platform frameworks and require specific external fixtures. The full legacy payload suite also expects an AnyPS5 checkout for its NID comparison oracle. Historical verification paths in DEVELOPMENT.md and rust/README.md refer to the original local workspace and are not bundled evidence or portable setup instructions.

Do not infer gameplay from a successful build or unit test run. No game executable is distributed here.
