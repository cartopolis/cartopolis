---
topic: apk-stale-so
triggers: [crates/cartopolis_android/build-apk.sh, deploy/build/apk.sh, libcartopolis_android.so, "stale .so", INSTALL_FAILED_UPDATE_INCOMPATIBLE]
updated: 2026-10-31
---

# `build-apk.sh` packages whatever `.so` it finds

The script packages `${CARGO_TARGET_DIR:-target}/<triple>/<profile>/libcartopolis_android.so`.
If cargo wrote somewhere else, it packages an older `.so` lying at that path,
signs it and reports success. On 2026-08-27 that cost an hour of emulator work
asking why a new log line was absent. A fix (`8befe05`) never landed.

Verify the artifact, because nothing else will tell you:

```bash
strings -a <target>/aarch64-linux-android/<profile>/libcartopolis_android.so | grep -c '<a string your change added>'
```

Each output directory also gets its own debug keystore, so switching directories
gives `INSTALL_FAILED_UPDATE_INCOMPATIBLE` (uninstall first). `adb install`
succeeding is not evidence the new code ran.
