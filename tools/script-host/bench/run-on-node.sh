# Both hosts, one machine: `ssh <node> bash tools/script-host/bench/run-on-node.sh`.
# Fetches Bun 1.4.4 into ~/.cache once. Every group runs in a process of its own,
# and a failing group does not stop the rest.
B=$HOME/.cache/bun-1.4.4/bun
if [ ! -x "$B" ]; then
  mkdir -p "$HOME/.cache/bun-1.4.4" && cd "$HOME/.cache/bun-1.4.4" || exit 1
  case "$(uname -s)-$(uname -m)" in Darwin-arm64) Z=bun-darwin-aarch64;; Linux-x86_64) Z=bun-linux-x64;; Linux-aarch64) Z=bun-linux-aarch64;; esac
  curl -fsSLO "https://github.com/oven-sh/bun/releases/download/bun-v1.4.4/$Z.zip" && unzip -oq "$Z.zip" && mv "$Z/bun" bun && cd - >/dev/null || exit 1
fi
echo "machine: $(uname -sm), $(sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo)"
cd "$(dirname "$0")/.." || exit 1
"$B" install --frozen-lockfile >/dev/null
"$B" test 2>&1 | tail -3
for group in "memory 200" "memory 2000" "cpu" "process 200" "process 2000"; do
  "$B" bench/bench.ts $group 2>&1 | grep '^{' || echo "{\"engine\":\"bun\",\"group\":\"$group\",\"failed\":true}"
done
cd bench/luau && cargo run --release --quiet 2>&1 | grep -E '^\{|panicked|error'
