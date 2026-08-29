-- The firebird: a burning thing that circles Groningen until somebody gets close.
--
-- It is the worked example for the authoring API and for everything protocol 20
-- added, and it is written to use each of those the way an author would rather
-- than the way a test would:
--
--   vfSetGeo      moves it across the map *durably* — vfSetPos's x and z are a
--                 client-frame echo, so a lap flown with those would be undone by
--                 the next re-anchor (see docs/notes/prim-scripting.md).
--   vfGeoScale    turns a radius in metres into an offset in degrees, which is
--                 the one bit of geodesy a script cannot sensibly do itself.
--   vfSensor      answers "is anybody near me?" in the only frame where that has
--                 an answer — an avatar's sim position is metres in its own
--                 client's anchored frame, and a prim's anchor is absolute.
--   vfParticles   the flame it flies with.
--   vfParticleBurst  the explosion, which is the same call fired once.
--
-- Install it with:  tools/author.py firebird
--
-- It ends by deleting itself, because that is what exploding means. Run the
-- installer again for another one.

-- Where it circles, how wide, and how long a lap takes.
local CENTRE_LAT, CENTRE_LNG = 53.2194, 6.5665
local RADIUS_M = 180.0
local LAP_S = 48.0
local ALTITUDE_M = 45.0

-- How near an avatar has to get, in metres. Comfortably inside the sim's sensor
-- ceiling and comfortably outside "you had to walk into it".
local NOTICE_M = 45.0

-- How long it shakes before it goes, and how far it rattles.
local SHAKE_S = 2.5
local SHAKE_M = 0.6

-- How long the blast is left standing before the prim removes itself. The
-- explosion is drawn by the client for its own lifetime; dying at the same
-- instant would take the emitter away mid-bang.
local LINGER_S = 2.0

-- Metres per degree here. Computed once: it changes with latitude and this thing
-- never leaves Groningen.
local M_PER_LAT, M_PER_LNG = vfGeoScale(CENTRE_LAT)

local state = "flying"
local marked = 0

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

function on_start()
    vfSetName("Firebird")
    -- A dark shell, so what you see is the fire and not a lit cylinder.
    vfSetColor(0.09, 0.07, 0.06)
    vfSetGlow(0.25)
    vfSetScale(1.6, 0.9, 1.6)
    -- Only the height: horizontal placement belongs to the geo anchor, which
    -- vfSetGeo writes below.
    local x, _, z = vfGetPos()
    vfSetPos(x, ALTITUDE_M, z)
    vfParticles(FLAME)
    vfShout("The firebird is up.")
    -- One tick at 20 Hz. The handler is a dozen instructions; the per-script
    -- slice is 20,000.
    vfSetTimerEvent(0.05)
end

-- Where on the circle it is at time `t`.
local function fly(t)
    local a = (t / LAP_S) * 2 * math.pi
    vfSetGeo(
        CENTRE_LAT + (RADIUS_M * math.cos(a)) / M_PER_LAT,
        CENTRE_LNG + (RADIUS_M * math.sin(a)) / M_PER_LNG
    )
    -- Nose along the tangent. Degrees, as vfSetRot takes and the edit dialog
    -- shows.
    vfSetRot(0, -math.deg(a), 0)
end

function on_timer()
    local now = vfGetTime()

    if state == "flying" then
        fly(now)
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
        vfDie()
    end
end
