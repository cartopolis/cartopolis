// firebird.lua beside this, line for line in JavaScript, for `script_engine = "bun"`. The
// comments explaining the design stay in the Lua original.
const CENTRE_LAT = 53.2194, CENTRE_LNG = 6.5665
const RADIUS_M = 180.0
const LAP_S = 48.0
const ALTITUDE_M = 45.0
const BEACON_LAT = 53.21935, BEACON_LNG = 6.56675
const HOVER_M = 9.0
const HOVER_DRIFT_M = 2.5
const NOTICE_M = 45.0
const SHAKE_S = 2.5
const SHAKE_M = 0.6
const LINGER_S = 2.0
const RELEASE_GRACE_S = 6.0
const CHANNEL = 4747
const [M_PER_LAT, M_PER_LNG] = geo_scale(CENTRE_LAT)

let state = 'patrol'
let marked = 0
let lap_epoch = 0
let calm_until = 0

const FLAME = {
    count: 120,
    life: 1.3,
    speed: 2.2,
    spread_deg: 26,
    gravity: 2.6,
    source_radius: 0.6,
    size_start: 1.1,
    size_end: 0.15,
    color_start: [5.0, 2.2, 0.5],
    color_end: [1.2, 0.1, 0.02],
    turbulence: 1.2,
}

function light_it() {
    set_alpha(1.0)
    set_glow(0.25)
    particles(FLAME)
}

function on_start() {
    set_name('Firebird')
    set_color(0.09, 0.07, 0.06)
    set_scale(1.6, 0.9, 1.6)
    const [x, , z] = get_pos()
    set_pos(x, ALTITUDE_M, z)
    light_it()
    listen(CHANNEL)
    shout('The firebird is up.')
    set_timer_event(0.05)
}

function fly(t) {
    const a = ((t - lap_epoch) / LAP_S) * 2 * Math.PI
    set_geo(CENTRE_LAT + (RADIUS_M * Math.cos(a)) / M_PER_LAT, CENTRE_LNG + (RADIUS_M * Math.sin(a)) / M_PER_LNG)
    set_rot(0, -(a * 180) / Math.PI, 0)
}

function hover(t) {
    const a = t * 0.7
    set_geo(BEACON_LAT + (HOVER_DRIFT_M * Math.cos(a)) / M_PER_LAT, BEACON_LNG + (HOVER_DRIFT_M * Math.sin(a)) / M_PER_LNG)
    const [x, , z] = get_pos()
    set_pos(x, HOVER_M + 0.6 * Math.sin(t * 1.7), z)
    set_rot(0, ((a * 180) / Math.PI) * 2.0, 0)
}

function report() {
    send(CHANNEL, state === 'summoned' ? 'here' : 'patrolling')
}

function on_message(channel, text, sender) {
    if (text === 'where') {
        report()
        return
    }
    if (state === 'summoned') {
        state = 'patrol'
        lap_epoch = get_time()
        calm_until = get_time() + RELEASE_GRACE_S
        const [x, , z] = get_pos()
        set_pos(x, ALTITUDE_M, z)
        shout('Back to the rounds.')
    } else if (state === 'patrol') {
        state = 'summoned'
        shout('Coming.')
    } else {
        return
    }
    report()
}

function on_timer() {
    const now = get_time()
    if (state === 'summoned') {
        hover(now)
        return
    }
    if (state === 'patrol') {
        fly(now)
        if (now >= calm_until) {
            const [id, who, distance] = sensor(NOTICE_M)
            if (id) {
                state = 'shaking'
                marked = now
                shout(`${who} is ${distance.toFixed(0)} m away. Stand back.`)
                FLAME.count = 200
                FLAME.speed = 4.0
                FLAME.size_start = 1.6
                particles(FLAME)
            }
        }
        return
    }
    if (state === 'shaking') {
        const t = now - marked
        const wobble = SHAKE_M * (t / SHAKE_S)
        const [lat, lng] = get_geo()
        set_geo(lat + (wobble * Math.sin(t * 47.0)) / M_PER_LAT, lng + (wobble * Math.cos(t * 53.0)) / M_PER_LNG)
        set_rot(6.0 * Math.sin(t * 61.0), 0, 6.0 * Math.cos(t * 59.0))
        if (t >= SHAKE_S) {
            state = 'blown'
            marked = now
            shout('...')
            particle_burst({
                count: 240,
                life: 1.4,
                speed: 18.0,
                spread_deg: 180,
                gravity: -9.0,
                source_radius: 1.0,
                size_start: 2.2,
                size_end: 0.3,
                color_start: [8.0, 3.6, 1.0],
                color_end: [0.9, 0.12, 0.02],
            })
            set_alpha(0.0)
            set_glow(0.0)
        }
        return
    }
    if (state === 'blown' && now - marked >= LINGER_S) {
        state = 'patrol'
        lap_epoch = now
        calm_until = now + RELEASE_GRACE_S
        FLAME.count = 120
        FLAME.speed = 2.2
        FLAME.size_start = 1.1
        const [x, , z] = get_pos()
        set_pos(x, ALTITUDE_M, z)
        fly(now)
        light_it()
        report()
    }
}
