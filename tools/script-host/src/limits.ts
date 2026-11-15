/**
 * The Luau host's numbers (`crates/simulator/src/scripting.rs`), carried over so
 * the two hosts are compared under one budget. Fuel counts the same two things
 * in both — loop iterations and function entries — so the values transfer.
 */
export const LIMITS = {
    /** `WORLD_FUEL_PER_TICK`: shared by every script, refilled each tick. */
    worldFuelPerTick: 200_000,
    /** `SCRIPT_FUEL_SLICE`: the most one script draws in one tick. */
    fuelSlice: 20_000,
    /** `LOAD_FUEL`: the top level's one-off budget. */
    loadFuel: 200_000,
    /** `MAX_SOURCE_BYTES`. */
    maxSourceBytes: 64 * 1024,
    /** `MAX_PENDING_EVENTS`. */
    maxPendingEvents: 32,
    /** `MAX_COMMANDS_PER_TICK`. */
    commandsPerTick: 64,
    /** `MIN_TIMER_INTERVAL`. */
    minTimerInterval: 0.05,
    /** `MAX_MESSAGES_PER_TICK`. */
    messagesPerTick: 8,
    /** `MAX_SOUNDS_PER_TICK`. */
    soundsPerTick: 2,
    /** `MAX_LISTEN_CHANNELS`. */
    maxListenChannels: 16,
    /** `MAX_TEXT`. */
    maxText: 500,
    whisperRadius: 10,
    // `crate::chat::SAY_RADIUS_M` and `SHOUT_RADIUS_M`.
    sayRadius: 50,
    shoutRadius: 300,
    /** `cartopolis_core::tone::MAX_HAND_RANGE_M`. */
    maxHandRange: 5,

    // ── New here: the ceilings Luau's allocator limit gave for free ──

    /** Longest string or array one built-in call may produce. */
    maxLength: 1 << 20,
    /** How far one slice may grow the heap before it is stopped. */
    sliceHeap: 16 << 20,
    /** What one script may hold between ticks, judged by the heap audit. */
    scriptRetained: 8 << 20,
} as const

/** `SCRIPT_EVENTS`: the handlers the host looks for by name. */
export const HANDLERS = ['on_start', 'on_touch', 'on_timer', 'on_message', 'on_hand'] as const
