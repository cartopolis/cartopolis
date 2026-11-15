// theremin.lua beside this, line for line in JavaScript, for `script_engine = "bun"`. The
// comments explaining the design stay in the Lua original.
const ROD = [0.35, 0.45, 0]
const LOOP = [-0.4, 0.05, 0]
const LOW = 130.81, HIGH = 1046.5
const PITCH_NEAR = 0.05, PITCH_FAR = 0.6
const VOLUME_NEAR = 0.03, VOLUME_FAR = 0.4
const REACH = 1.5
const GONE_AFTER = 1.0
const IDLE_NAME = 'Theremin — walk up and play'

let player = null
let last_seen = 0
let shown = 0

function distance(x, y, z, p) {
    const dx = x - p[0], dy = y - p[1], dz = z - p[2]
    return Math.sqrt(dx * dx + dy * dy + dz * dz)
}

function closeness(d, near, far) {
    return Math.min(1, Math.max(0, (far - d) / (far - near)))
}

function free() {
    player = null
    stop_tone()
    set_name(IDLE_NAME)
    set_glow(0)
    shown = 0
}

function on_start() {
    sense_hands(REACH)
    free()
    set_timer_event(0.25)
}

function on_hand(id, name, x, y, z, precise) {
    if (player === null) {
        player = id
        set_name('Theremin — ' + name + ' is playing')
    }
    if (id !== player) return
    last_seen = get_time()
    const t = closeness(distance(x, y, z, ROD), PITCH_NEAR, PITCH_FAR)
    const pitch = LOW * (HIGH / LOW) ** t
    let gain = 1 - closeness(distance(x, y, z, LOOP), VOLUME_NEAR, VOLUME_FAR)
    if (!precise) gain = gain * 0.5
    set_tone({voice: 'sine', pitch, gain, glide: 0.08, vibrato: 0.15, range: 25})
    const glow = Math.floor(gain * 8 + 0.5) / 10
    if (glow !== shown) {
        set_glow(glow)
        shown = glow
    }
}

function on_timer() {
    if (player !== null && get_time() - last_seen > GONE_AFTER) free()
}
