// A theremin on the Grote Markt: walk up and play it with your hand.
//
// The worked example for **hands** and **tones**, and the whole instrument is
// this script. The engine knows nothing about theremins: it reports the hands
// near the cabinet (`sense_hands`, `on_hand` — only from people who switched on
// high-precision tracking) and sounds whatever tone the script asks for
// (`set_tone`). Who gets to play, and how a hand becomes a note, is below.
//
// Like the real thing it has two antennas: an upright rod on the right — the
// nearer the hand, the higher the note — and a loop on the left — the nearer
// the hand, the quieter. They are the brass prims `content.rs` places beside
// the cabinet, at the two points below; move one and move the other.

// The pitch antenna, metres from the cabinet's centre.
const ROD = [0.35, 0.45, 0]
// The volume antenna.
const LOOP = [-0.4, 0.05, 0]
// C3 with the hand 60 cm from the rod, C6 at 5 cm.
const LOW = 130.81, HIGH = 1046.5
const PITCH_NEAR = 0.05, PITCH_FAR = 0.6
// Silent with the hand on the loop, full 40 cm away.
const VOLUME_NEAR = 0.03, VOLUME_FAR = 0.4
// Hands are sensed this far from the cabinet's centre: both antennas' fields
// and a little more.
const REACH = 1.5
// A player whose hand has not arrived for this long has walked away.
const GONE_AFTER = 1.0
const IDLE_NAME = 'Theremin — walk up and play'

// Who is playing, and when their hand last arrived. One player at a time:
// the first hand to arrive plays, and everybody else's is ignored until they
// have gone.
let player = null
let last_seen = 0
// The glow last written. The tone follows every hand; the glow is a whole-prim
// update to every client, so it is only written when it visibly changes.
let shown = 0

function distance(x, y, z, p) {
    const dx = x - p[0], dy = y - p[1], dz = z - p[2]
    return Math.sqrt(dx * dx + dy * dy + dz * dz)
}

// 0 at `far` and beyond, 1 at `near` and closer.
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
    // Exponential in distance, so equal hand movements are equal musical
    // intervals: that is what makes a theremin learnable.
    const t = closeness(distance(x, y, z, ROD), PITCH_NEAR, PITCH_FAR)
    const pitch = LOW * (HIGH / LOW) ** t
    let gain = 1 - closeness(distance(x, y, z, LOOP), VOLUME_NEAR, VOLUME_FAR)
    // A camera standing in for a hand (a developer at a desktop) is a toy;
    // quieter, so nobody mistakes it for the real thing.
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
