java := "/Library/Java/JavaVirtualMachines/temurin-27.jdk/Contents/Home/bin/java"
build_jar := "./references/Digital_release/Digital.jar"

default:
    just --list

run:
    {{ java }} -jar {{ build_jar }} 

build_reference:
    echo "TODO"

# Run the native Rust scaffold (the `run` recipe remains the Java reference).
run-rust:
    cargo run --locked --manifest-path reimplementation/Cargo.toml

build-rust:
    cargo build --locked --manifest-path reimplementation/Cargo.toml

# Compatibility with the original misspelled recipe.
build_reimplemtation: build-rust

check-rust:
    cargo fmt --manifest-path reimplementation/Cargo.toml --all -- --check
    cargo check --locked --manifest-path reimplementation/Cargo.toml --all-targets
    cargo test --locked --manifest-path reimplementation/Cargo.toml
    cargo clippy --locked --manifest-path reimplementation/Cargo.toml --all-targets -- -D warnings

# Build and copy the generated icon/brand PNGs to target/asset-preview (git-ignored).
assets-preview:
    DIGITAL_ASSET_PREVIEW_DIR="$PWD/target/asset-preview" cargo build --locked --manifest-path reimplementation/Cargo.toml
    @echo "Asset previews: $PWD/target/asset-preview"
