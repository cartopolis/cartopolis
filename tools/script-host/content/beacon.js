// crates/simulator/content/beacon.lua, line for line in JavaScript. The
// comments explaining the design stay in the Lua original.
const CHANNEL = 4747
let status = '?'

function label() {
    if (status === 'here') return 'Beacon — press to release the firebird'
    if (status === 'patrolling') return 'Beacon — press to summon the firebird'
    return 'Beacon — press to call the firebird'
}

const MARKER = {
    count: 70,
    life: 1.6,
    speed: 1.1,
    spread_deg: 16,
    gravity: 1.6,
    source_radius: 0.35,
    size_start: 0.55,
    size_end: 0.08,
    color_start: [4.0, 1.6, 0.3],
    color_end: [0.9, 0.15, 0.03],
    alpha: 0.8,
    turbulence: 0.7,
}

function on_start() {
    set_name(label())
    set_color(1.0, 0.42, 0.08)
    set_glow(0.35)
    set_full_bright(true)
    set_scale(1.6, 2.6, 0.28)
    const [x, , z] = get_pos()
    set_pos(x, 1.3, z)
    particles(MARKER)
    listen(CHANNEL)
    send(CHANNEL, 'where')
}

function on_touch(avatar_id, avatar_name) {
    send(CHANNEL, 'toggle ' + avatar_name)
    say(avatar_name + ' presses the beacon.')
    set_glow(1.0)
    sleep(0.25)
    set_glow(0.35)
}

function on_message(channel, text, sender) {
    if (text === 'here' || text === 'patrolling') {
        status = text
        set_name(label())
        if (status === 'here') {
            set_color(1.0, 0.72, 0.3)
            set_glow(0.7)
        } else {
            set_color(1.0, 0.42, 0.08)
            set_glow(0.35)
        }
    }
}
