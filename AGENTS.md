# Project direction

The user explicitly requires Rust for future implementation work. Implement new CPU translation, runtime, GPU backend orchestration, tooling and tests in Rust. Apple GPU shader programs may use Metal Shading Language. Keep platform ABI glue narrowly scoped.

The active CPU translator and native CPU verification runner are in `rust/`. Existing Python translators and C++/Objective-C++ implementations are migration references and regression oracles; do not add new product functionality to them. Migrate subsystems incrementally, with behavior checks before switching their callers. Do not silently fall back to Python from the Rust path.

Target native ARM64 execution and the Apple GPU, with a 16 GB unified-memory design budget. Preserve guarded guest memory, explicit unsupported-instruction boundaries and instruction budgets. Do not equate Rust compilation, synthetic tests or successful homebrew initialization with PS5 game compatibility, native-port performance, or completed gameplay. The upstream AnyPS5 checkout remains separate from this prototype.

See `rust/README.md` for the current migration boundary and reproducible checks.
