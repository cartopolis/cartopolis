# The simulator's scripting tests on a node, Bun ones included where Bun 1.4 is.
cargo test -p cartopolis_simulator --lib -- scripting:: content:: config:: 2>&1 | grep -vE '^\s+(Compiling|Downloaded|Downloading)' | tail -80
