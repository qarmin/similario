set export := true

RUST_LOG := "debug"

gui *args:
    cargo run --bin similario_gui -- {{args}}

guir *args:
    cargo run --release --bin similario_gui -- {{args}}

cli *args:
    cargo run --bin similario -- {{args}}

clir *args:
    cargo run --release --bin similario -- {{args}}

cli-asan *args:
    ASAN_OPTIONS="symbolize=1:detect_leaks=0" \
    ASAN_SYMBOLIZER_PATH=$(which llvm-symbolizer) \
    RUSTFLAGS="-Zsanitizer=address" \
    cargo +nightly run --target x86_64-unknown-linux-gnu --bin similario -- {{args}}

build:
    cargo build

buildr:
    cargo build --release

fmt:
    cargo fmt --all

fix:
    grep -rlZ --include="*.rs" "─" . | xargs -0 sed -i 's/─//g' || true
    cargo +nightly fmt --all
    cargo clippy --fix --allow-dirty --allow-staged --all-features --all-targets --workspace
    cargo +nightly fmt --all
    cargo fmt --all

upgrade:
    cargo +nightly -Z unstable-options update --breaking
    cargo update

install:
    cargo install --path similario_cli --locked
    cargo install --path similario_gui --locked

cache:
    xdg-open ~/.cache/similario/signatures

cache-clean:
    cargo run --bin similario -- clean-cache --older-than-days 30

samply-cli *args:
    cargo build --bin similario
    samply record target/debug/similario {{args}}

samply-gui *args:
    cargo build --bin similario_gui
    samply record target/debug/similario_gui {{args}}

bloat:
    cargo bloat --release --bin similario -n 30
    cargo bloat --release --bin similario_gui -n 30

bloat-crates:
    cargo bloat --release --crates --bin similario
    cargo bloat --release --crates --bin similario_gui

heaptrack bin:
    cargo build --profile fast_release --bin {{bin}}
    heaptrack target/fast_release/{{bin}}
    heaptrack_gui --appimage-extract-and-run "$(ls -t *.zst | head -n1)"

clean:
    cargo clean

setup-sanitizer:
    rustup install nightly
    rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu
    rustup component add llvm-tools-preview --toolchain nightly-x86_64-unknown-linux-gnu

setup-profiling:
    rustup component add llvm-tools-preview
    cargo install cargo-bloat flamegraph samply

prepare-test-videos:
    bash tests/prepare_test_videos.sh

test-duplicates:
    cargo test --package similario_core --test integration_duplicates -- --nocapture

binaries:
    rm -rf binaries || true
    mkdir binaries
    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
    cp target/x86_64-unknown-linux-gnu/release/similario_gui binaries/
    cp target/x86_64-unknown-linux-gnu/release/similario_cli binaries/
