-- The football in the Euroborg: a physical prim (`docs/notes/physical-prims.md`)
-- that referees itself.
--
-- The physics is the simulator's, not this script's: run into the ball and you
-- dribble it, use the kick emote (5) beside it and it flies the way you face.
-- What the script adds is the game around it — it notices the ball crossing a
-- goal line between the posts, keeps the score, says so, and puts the ball back
-- on the centre spot. It also fetches the ball back when it leaves the stadium.
--
-- The pitch is described by the two points the content pack built the goals on
-- (the middle of each goal line, from OpenStreetMap way 185043967), so this file
-- and `content.rs` agree on where the goals are by having the same two numbers.

local NORTH = { lat = 53.20661975, lng = 6.5916658 }
local SOUTH = { lat = 53.20566315, lng = 6.5916427 }

local GOAL_HALF_WIDTH = 3.66 -- metres either side of the middle of the line
local GOAL_HEIGHT = 2.44
local BALL_RADIUS = 0.11
-- Further than this outside the pitch and the ball has left the stadium.
local OUT_OF_PLAY = 40

local centre = {
    lat = (NORTH.lat + SOUTH.lat) / 2,
    lng = (NORTH.lng + SOUTH.lng) / 2,
}
local mlat, mlng = geo_scale(centre.lat)

-- The pitch's own axes in metres: `along` points at the north goal, `across` is
-- square to it, pointing roughly east.
local axis_e = (NORTH.lng - SOUTH.lng) * mlng
local axis_n = (NORTH.lat - SOUTH.lat) * mlat
local length = math.sqrt(axis_e * axis_e + axis_n * axis_n)
axis_e, axis_n = axis_e / length, axis_n / length
local HALF_LENGTH = length / 2

local north_score, south_score = 0, 0
-- Seconds until the ball goes back to the centre spot, or nil when in play.
local restart_in = nil

local function on_pitch()
    local lat, lng = get_geo()
    local e = (lng - centre.lng) * mlng
    local n = (lat - centre.lat) * mlat
    return e * axis_e + n * axis_n, e * axis_n - n * axis_e
end

local function score_line()
    return "North " .. north_score .. " – " .. south_score .. " South"
end

local function place_on_centre_spot()
    set_geo(centre.lat, centre.lng)
    local x, _, z = get_pos()
    -- Dropped from a little height, so a restart reads as one.
    set_pos(x, 1.0, z)
    set_vel(0, 0, 0)
end

function on_start()
    set_name("Football — run into it to dribble, kick emote (5) to shoot")
    set_color(1.0, 1.0, 1.0)
    set_physical(true)
    set_timer_event(0.1)
end

function on_touch(_, avatar_name)
    say(avatar_name .. " puts the ball on the centre spot. " .. score_line())
    restart_in = nil
    place_on_centre_spot()
end

function on_timer()
    if restart_in then
        restart_in = restart_in - 0.1
        if restart_in <= 0 then
            restart_in = nil
            place_on_centre_spot()
        end
        return
    end

    local along, across = on_pitch()
    local _, y = get_pos()
    local between_posts = math.abs(across) < GOAL_HALF_WIDTH - BALL_RADIUS
        and y < GOAL_HEIGHT - BALL_RADIUS

    -- Wholly over the line, between the posts and under the bar.
    if between_posts and along > HALF_LENGTH + BALL_RADIUS then
        south_score = south_score + 1
        shout("GOAL at the north end! " .. score_line())
        play_sound("bell")
        restart_in = 3
    elseif between_posts and along < -(HALF_LENGTH + BALL_RADIUS) then
        north_score = north_score + 1
        shout("GOAL at the south end! " .. score_line())
        play_sound("bell")
        restart_in = 3
    elseif math.abs(along) > HALF_LENGTH + OUT_OF_PLAY or math.abs(across) > 34 + OUT_OF_PLAY then
        say("The ball left the stadium; back to the centre spot.")
        restart_in = 1
    end
end
