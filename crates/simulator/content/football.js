// football.lua beside this, line for line in JavaScript, for `script_engine = "bun"`. The
// comments explaining the design stay in the Lua original.
const NORTH = {lat: 53.20661975, lng: 6.5916658}
const SOUTH = {lat: 53.20566315, lng: 6.5916427}
const GOAL_HALF_WIDTH = 3.66
const GOAL_HEIGHT = 2.44
const BALL_RADIUS = 0.11
const OUT_OF_PLAY = 4.5
const HALF_WIDTH = 34

const centre = {lat: (NORTH.lat + SOUTH.lat) / 2, lng: (NORTH.lng + SOUTH.lng) / 2}
const [mlat, mlng] = geo_scale(centre.lat)
let axis_e = (NORTH.lng - SOUTH.lng) * mlng
let axis_n = (NORTH.lat - SOUTH.lat) * mlat
const length = Math.sqrt(axis_e * axis_e + axis_n * axis_n)
axis_e = axis_e / length
axis_n = axis_n / length
const HALF_LENGTH = length / 2

let north_score = 0, south_score = 0
let restart_in = null

function on_pitch() {
    const [lat, lng] = get_geo()
    const e = (lng - centre.lng) * mlng
    const n = (lat - centre.lat) * mlat
    return [e * axis_e + n * axis_n, e * axis_n - n * axis_e]
}

function score_line() {
    return 'North ' + north_score + ' – ' + south_score + ' South'
}

function place_on_centre_spot() {
    set_geo(centre.lat, centre.lng)
    const [x, , z] = get_pos()
    set_pos(x, 1.0, z)
    set_vel(0, 0, 0)
}

function on_start() {
    set_name('Football — run into it to dribble, kick emote (5) to shoot')
    set_color(1.0, 1.0, 1.0)
    set_physical(true)
    set_timer_event(0.1)
}

function on_touch(_, avatar_name) {
    say(avatar_name + ' puts the ball on the centre spot. ' + score_line())
    restart_in = null
    place_on_centre_spot()
}

function on_timer() {
    if (restart_in !== null) {
        restart_in = restart_in - 0.1
        if (restart_in <= 0) {
            restart_in = null
            place_on_centre_spot()
        }
        return
    }
    const [along, across] = on_pitch()
    const [, y] = get_pos()
    const between_posts = Math.abs(across) < GOAL_HALF_WIDTH - BALL_RADIUS && y < GOAL_HEIGHT - BALL_RADIUS
    if (between_posts && along > HALF_LENGTH + BALL_RADIUS) {
        south_score = south_score + 1
        shout('GOAL at the north end! ' + score_line())
        play_sound('bell')
        restart_in = 3
    } else if (between_posts && along < -(HALF_LENGTH + BALL_RADIUS)) {
        north_score = north_score + 1
        shout('GOAL at the south end! ' + score_line())
        play_sound('bell')
        restart_in = 3
    } else if (Math.abs(along) > HALF_LENGTH + OUT_OF_PLAY || Math.abs(across) > HALF_WIDTH + OUT_OF_PLAY) {
        say('The ball went over the boards; back to the centre spot.')
        restart_in = 1
    }
}
