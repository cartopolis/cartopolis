// Trunk initializer: drives the HTML loader while the wasm module downloads and
// compiles (`data-initializer` on the `rel="rust"` link in index.html).
//
// This is deliberately *not* a splash. The splash is Bevy's, and it cannot exist
// until the engine is running — which is precisely the several seconds this
// covers, since the module is ~70 MB. So the job here is narrow: say honestly
// how far along the load is, and say clearly when it has failed.
//
// Trunk calls `onProgress({current, total})` as bytes arrive. `total` is absent
// whenever the response has no usable Content-Length — which is the normal case
// for a compressed transfer — so the bar falls back to indeterminate rather than
// inventing a percentage it cannot know.
export default function initializer() {
  const el = (id) => document.getElementById(id);
  // `force` is what keeps a diagnosis on screen. The sanity checks in index.html
  // run concurrently with the transfer, so a fatal one (no GPU adapter) lands
  // mid-download and the very next `onProgress` tick — a few hundred ms later —
  // would paint "Downloading engine… 41.2 MB" back over it. Once the boot has
  // failed, only the failure path may write here.
  const set = (state, text, force) => {
    if (window.__boot.failed && !force) return;
    const s = el("status");
    if (s) s.textContent = text;
    const root = el("loader");
    if (root) root.dataset.state = state;
  };

  return {
    onStart: () => {
      set("loading", "Downloading engine…");
    },

    onProgress: ({ current, total }) => {
      const bar = el("bar");
      if (!bar) return;
      const mb = (n) => (n / 1e6).toFixed(1);
      if (total) {
        const pct = Math.max(0, Math.min(100, (current / total) * 100));
        bar.style.width = pct.toFixed(1) + "%";
        bar.classList.remove("indeterminate");
        set("loading", `Downloading engine… ${mb(current)} / ${mb(total)} MB`);
      } else {
        // No Content-Length: show motion and the byte count, not a fake percentage.
        bar.classList.add("indeterminate");
        set("loading", `Downloading engine… ${mb(current)} MB`);
      }
    },

    onComplete: () => {
      // Download finished; the browser now compiles and instantiates, which for a
      // module this size is seconds more with no progress signal available.
      const bar = el("bar");
      if (bar) {
        bar.style.width = "100%";
        bar.classList.add("indeterminate");
      }
      set("compiling", "Starting engine…");
    },

    onSuccess: () => {
      // Do NOT hide the loader here. `onSuccess` fires when the module has
      // instantiated, not when anything has been drawn — hiding now would show a
      // blank canvas until Bevy's own splash appears. index.html hides it on the
      // first real frame instead.
      set("running", "Ready");
    },

    onFailure: (error) => {
      window.__boot.failed = true;
      set("failed", "Failed to start: " + (error && error.message ? error.message : error), true);
      console.error("wasm initialization failed:", error);
    },
  };
}
