// A theremin on the Grote Markt: walk up and play it with your hand.
//
// The worked example for **hands** and **tones**, and the whole instrument is
// this script. The engine knows nothing about theremins: it reports the hands
// near the cabinet (`sense_hands`, `on_hand` — only from people who switched on
// high-precision tracking) and sounds whatever tone the script asks for
// (`set_tone`). Who gets to play, and how a hand becomes a note, is below.
//
// It has two antennas, an upright rod on the right and a loop on the left: the
// brass prims `content.rs` places beside the cabinet, at the two points below;
// move one and move the other. Move your hand toward the rod and the note
// rises; lower it toward the loop and the note fades.
//
// **The note is played by how the hand moves, not by where it is.** A phone
// knows how it moved to centimetres, but where it stands only as well as the
// compass heading it was put on the map with when tracking locked — even on
// an RTK fix, a few degrees off becomes metres off a few steps later. A
// theremin that read the hand's distance to
// the rod (as this one did until 2026-11-22) was silent for a hand reported two
// metres off, and for one reported nearly right it put the whole arm's sweep in
// one corner of the range. So the hand that starts playing sets the middle
// note where it is, and the range follows the hand when it is carried past an
// end: every hand reaches every note.

// The pitch antenna, metres from the cabinet's centre.
const ROD = [0.35, 0.45, 0]
// The volume antenna.
const LOOP = [-0.4, 0.05, 0]
// C3 to C6, three octaves, across this much hand travel toward the rod: 20 cm
// an octave, so the whole range is a sideways sweep of the forearm.
const LOW = 130.81, HIGH = 1046.5
const SPAN = 0.6
// Full volume at the height the hand started at; lowered this far, silent.
const FADE_FROM = 0.05, SILENT_AT = 0.3
// The hand may rise this far above its starting height before the volume's
// reference follows it up, so a lowered hand is always measured from the top.
const RISE = 0.1
// Hands are sensed this far from the cabinet's centre: the most the engine
// allows, because a hand at the cabinet may be reported metres off.
const REACH = 5
// A player whose hand has not arrived for this long has walked away.
const GONE_AFTER = 1.0
const IDLE_NAME = 'Theremin — walk up and play'
// How it shows it is being played: notes rising from the cabinet.
const NOTES = {
    count: 24,
    life: 1.8,
    speed: 0.5,
    spread_deg: 25,
    gravity: -0.3,
    source_radius: 0.2,
    size_start: 0.09,
    size_end: 0.02,
    color_start: [0.6, 1.8, 2.4],
    color_end: [0.2, 0.5, 1.2],
    alpha: 0.8,
    turbulence: 0.6,
}
// Idle, it glows faintly, so it reads as something that does something.
const IDLE_GLOW = 0.15

// Which way is toward the rod: from the loop to it, level.
const AXIS = (() => {
    const dx = ROD[0] - LOOP[0], dz = ROD[2] - LOOP[2]
    const l = Math.sqrt(dx * dx + dz * dz)
    return [dx / l, dz / l]
})()

// Who is playing, and when their hand last arrived. One player at a time:
// the first hand to arrive plays, and everybody else's is ignored until they
// have gone.
let player = null
let last_seen = 0
// Where on the axis the lowest note is, and the height the volume is measured
// from: both set by the player's first hand, and dragged by it.
let low_at = 0
let top = 0
// The glow last written. The tone follows every hand; the glow is a whole-prim
// update to every client, so it is only written when it visibly changes.
let shown = 0

function free() {
    player = null
    stop_tone()
    particles(null)
    set_name(IDLE_NAME)
    set_glow(IDLE_GLOW)
    shown = IDLE_GLOW
}

function on_start() {
    sense_hands(REACH)
    free()
    set_timer_event(0.25)
}

function on_hand(id, name, x, y, z, precise) {
    const along = x * AXIS[0] + z * AXIS[1]
    if (player === null) {
        player = id
        // The hand starts on the middle note, at full volume.
        low_at = along - SPAN / 2
        top = y
        set_name('Theremin — ' + name + ' is playing')
        particles(NOTES)
        play_sound('chime', 0.4, 10)
    }
    if (id !== player) return
    last_seen = get_time()
    // Carried past an end, the range comes along.
    low_at = Math.min(along, Math.max(along - SPAN, low_at))
    top = Math.max(top, y - RISE)
    // Exponential in distance, so equal hand movements are equal musical
    // intervals: that is what makes a theremin learnable.
    const t = (along - low_at) / SPAN
    const pitch = LOW * (HIGH / LOW) ** t
    const lowered = top - y
    let gain = Math.min(1, Math.max(0, (SILENT_AT - lowered) / (SILENT_AT - FADE_FROM)))
    // A camera standing in for a hand (a developer at a desktop) is a toy;
    // quieter, so nobody mistakes it for the real thing.
    if (!precise) gain = gain * 0.5
    set_tone({voice: 'sine', pitch, gain, glide: 0.08, vibrato: 0.15, range: 25})
    // Bright while it sounds, never darker than idle.
    const glow = Math.max(IDLE_GLOW, Math.floor(gain * 8 + 0.5) / 8)
    if (glow !== shown) {
        set_glow(glow)
        shown = glow
    }
}

function on_timer() {
    if (player !== null && get_time() - last_seen > GONE_AFTER) free()
}
