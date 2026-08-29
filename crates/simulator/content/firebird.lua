-- The firebird: a burning thing that circles Groningen, comes when the beacon on
-- the Grote Markt calls it, and goes off if you sneak up on it.
--
-- It is the worked example for everything protocol 20 and 21 added, and it uses
-- each of those the way an author would rather than the way a test would:
--
--   vfSetGeo      moves it across the map *durably* — vfSetPos's x and z are a
--                 client-frame echo, so a lap flown with those would be undone by
--                 the next re-anchor (see docs/notes/prim-scripting.md).
--   vfGeoScale    turns a radius in metres into an offset in degrees, which is
--                 the one bit of geodesy a script cannot sensibly do itself.
--   vfSensor      answers "is anybody near me?" in the only frame where that has
--                 an answer — an avatar's sim position is metres in its own
--                 client's anchored frame, and a prim's anchor is absolute.
--   vfListen /    the channel the beacon presses arrive on. Nothing else can
--   vfSend        reach another prim: every other call acts on this one.
--   vfParticles   the flame it flies with.
--   vfParticleBurst  the explosion, which is the same call fired once.
--
-- Installed with the beacon by:  tools/author.py install firebird
--
-- **It reforms rather than dying.** The first version ended on vfDie, which is
-- the honest reading of "it explodes" and is also a machine that works once: a
-- summon button is pointless if the thing it summons deleted itself the first
-- time somebody walked near it. So the blast is followed by a dark interval and
-- then it is back on its circle. `vfDie()` in place of the reform below is a
-- one-line change if a one-shot is what you want.

-- Where it circles, how wide, how long a lap takes, and how high it flies.
local CENTRE_LAT, CENTRE_LNG = 53.2194, 6.5665
local RADIUS_M = 180.0
local LAP_S = 48.0
local ALTITUDE_M = 45.0

-- Where it hovers when it has been called in. The beacon's own spot, a little to
-- one side and low enough to be worth looking at.
local BEACON_LAT, BEACON_LNG = 53.21935, 6.56675
local HOVER_M = 9.0
local HOVER_DRIFT_M = 2.5

-- How near an avatar has to get to startle it, in metres. Comfortably inside the
-- sim's sensor ceiling and comfortably outside "you had to walk into it".
local NOTICE_M = 45.0

-- How long it shakes before it goes, and how far it rattles.
local SHAKE_S = 2.5
local SHAKE_M = 0.6

-- How long the blast is left standing before the shell reforms. The explosion is
-- drawn by the client for its own lifetime; reforming at the same instant would
-- take the emitter away mid-bang.
local LINGER_S = 2.0

-- How long after being released it is too busy climbing away to be startled.
-- Without this, pressing the beacon a second time while standing next to it sends
-- the firebird out and it immediately explodes in your face — which is a fair
-- reading of the rules and is not what anybody pressing a button twice meant.
local RELEASE_GRACE_S = 6.0

-- The channel the beacon is on.
local CHANNEL = 4747

-- Metres per degree here. Computed once: it changes with latitude and this thing
-- never leaves Groningen.
local M_PER_LAT, M_PER_LNG = vfGeoScale(CENTRE_LAT)

local state = "patrol"
local marked = 0
-- Simulated time the patrol is measured from, so being summoned and released does
-- not teleport it round its own circle.
local lap_epoch = 0
local calm_until = 0

-- A flame: hot core, rising, curling. Every field left out keeps the fire
-- preset's value.
local FLAME = {
    count = 120,
    life = 1.3,
    speed = 2.2,
    spread = 26,
    gravity = 2.6,
    radius = 0.6,
    size = 1.1,
    end_size = 0.15,
    color = { 5.0, 2.2, 0.5 },
    end_color = { 1.2, 0.1, 0.02 },
    turbulence = 1.2,
}

local function light_it()
    vfSetAlpha(1.0)
    vfSetGlow(0.25)
    vfParticles(FLAME)
end

function on_start()
    vfSetName("Firebird")
    -- A dark shell, so what you see is the fire and not a lit cylinder.
    vfSetColor(0.09, 0.07, 0.06)
    vfSetScale(1.6, 0.9, 1.6)
    -- Only the height: horizontal placement belongs to the geo anchor, which
    -- vfSetGeo writes below.
    local x, _, z = vfGetPos()
    vfSetPos(x, ALTITUDE_M, z)
    light_it()
    vfListen(CHANNEL)
    vfShout("The firebird is up.")
    -- One tick at 20 Hz. The handler is a few dozen instructions; the per-script
    -- slice is 20,000.
    vfSetTimerEvent(0.05)
end

-- Where on the circle it is at time `t`.
local function fly(t)
    local a = ((t - lap_epoch) / LAP_S) * 2 * math.pi
    vfSetGeo(
        CENTRE_LAT + (RADIUS_M * math.cos(a)) / M_PER_LAT,
        CENTRE_LNG + (RADIUS_M * math.sin(a)) / M_PER_LNG
    )
    -- Nose along the tangent. Degrees, as vfSetRot takes and the edit dialog
    -- shows.
    vfSetRot(0, -math.deg(a), 0)
end

-- Hovering over the beacon, turning slowly and bobbing.
local function hover(t)
    local a = t * 0.7
    vfSetGeo(
        BEACON_LAT + (HOVER_DRIFT_M * math.cos(a)) / M_PER_LAT,
        BEACON_LNG + (HOVER_DRIFT_M * math.sin(a)) / M_PER_LNG
    )
    local x, _, z = vfGetPos()
    vfSetPos(x, HOVER_M + 0.6 * math.sin(t * 1.7), z)
    vfSetRot(0, math.deg(a) * 2.0, 0)
end

-- What the beacon's label is asking about.
local function report()
    if state == "summoned" then
        vfSend(CHANNEL, "here")
    else
        vfSend(CHANNEL, "patrolling")
    end
end

function on_message(channel, text, sender)
    if text == "where" then
        report()
        return
    end
    -- Anything else on this channel is a press. The beacon does not say which way
    -- to go, on purpose — only this script knows what it is currently doing, and
    -- two objects each holding half a state machine is how they come apart.
    if state == "summoned" then
        state = "patrol"
        -- Re-base the lap so it rejoins the circle where it left it rather than
        -- wherever the clock happens to have got to.
        lap_epoch = vfGetTime()
        calm_until = vfGetTime() + RELEASE_GRACE_S
        local x, _, z = vfGetPos()
        vfSetPos(x, ALTITUDE_M, z)
        vfShout("Back to the rounds.")
    elseif state == "patrol" then
        state = "summoned"
        vfShout("Coming.")
    else
        -- Mid-explosion. It will report again when it reforms; saying nothing is
        -- better than promising something that is currently in pieces.
        return
    end
    report()
end

function on_timer()
    local now = vfGetTime()

    if state == "summoned" then
        hover(now)
        return
    end

    if state == "patrol" then
        fly(now)
        if now >= calm_until then
            local id, who, distance = vfSensor(NOTICE_M)
            if id then
                state = "shaking"
                marked = now
                vfShout(string.format("%s is %.0f m away. Stand back.", who, distance))
                -- Wind the flame up: the fire is the warning.
                FLAME.count = 200
                FLAME.speed = 4.0
                FLAME.size = 1.6
                vfParticles(FLAME)
            end
        end
        return
    end

    if state == "shaking" then
        local t = now - marked
        -- Rattle in place. In *geo*, not in vfSetPos: horizontal position is the
        -- anchor's, so a jitter written to x/z would be quietly discarded by
        -- every client the moment the map re-anchored — which reads as a shake
        -- that works until it doesn't.
        local wobble = SHAKE_M * (t / SHAKE_S)
        local lat, lng = vfGetGeo()
        vfSetGeo(
            lat + (wobble * math.sin(t * 47.0)) / M_PER_LAT,
            lng + (wobble * math.cos(t * 53.0)) / M_PER_LNG
        )
        vfSetRot(6.0 * math.sin(t * 61.0), 0, 6.0 * math.cos(t * 59.0))
        if t >= SHAKE_S then
            state = "blown"
            marked = now
            vfShout("...")
            vfParticleBurst({
                count = 240,
                life = 1.4,
                speed = 18.0,
                spread = 180,
                gravity = -9.0,
                radius = 1.0,
                size = 2.2,
                end_size = 0.3,
                color = { 8.0, 3.6, 1.0 },
                end_color = { 0.9, 0.12, 0.02 },
            })
            -- The shell goes, the blast stays: what is left on screen is the
            -- explosion rather than a cylinder standing inside one.
            vfSetAlpha(0.0)
            vfSetGlow(0.0)
        end
        return
    end

    if state == "blown" and now - marked >= LINGER_S then
        -- Reform, out on the circle rather than where it went off — the person
        -- who startled it is standing there.
        state = "patrol"
        lap_epoch = now
        calm_until = now + RELEASE_GRACE_S
        FLAME.count = 120
        FLAME.speed = 2.2
        FLAME.size = 1.1
        local x, _, z = vfGetPos()
        vfSetPos(x, ALTITUDE_M, z)
        fly(now)
        light_it()
        report()
    end
end
