-- A theremin on the Grote Markt: walk up and play it with your hand.
--
-- The worked example for **hands** and **tones**, and the whole instrument is
-- this script. The engine knows nothing about theremins: it reports the hands
-- near the cabinet (`sense_hands`, `on_hand` — only from people who switched on
-- high-precision tracking) and sounds whatever tone the script asks for
-- (`set_tone`). Who gets to play, and how a hand becomes a note, is below.
--
-- Like the real thing it has two antennas: an upright rod on the right — the
-- nearer the hand, the higher the note — and a loop on the left — the nearer
-- the hand, the quieter. They are the brass prims `content.rs` places beside
-- the cabinet, at the two points below; move one and move the other.

local ROD = {0.35, 0.45, 0}  -- the pitch antenna, metres from the cabinet's centre
local LOOP = {-0.4, 0.05, 0} -- the volume antenna

-- C3 with the hand 60 cm from the rod, C6 at 5 cm.
local LOW, HIGH = 130.81, 1046.5
local PITCH_NEAR, PITCH_FAR = 0.05, 0.6
-- Silent with the hand on the loop, full 40 cm away.
local VOLUME_NEAR, VOLUME_FAR = 0.03, 0.4

-- Hands are sensed this far from the cabinet's centre: both antennas' fields
-- and a little more.
local REACH = 1.5
-- A player whose hand has not arrived for this long has walked away.
local GONE_AFTER = 1.0

local IDLE_NAME = "Theremin — walk up and play"

-- Who is playing, and when their hand last arrived. One player at a time:
-- the first hand to arrive plays, and everybody else's is ignored until they
-- have gone.
local player = nil
local last_seen = 0

-- The glow last written. The tone follows every hand; the glow is a whole-prim
-- update to every client, so it is only written when it visibly changes.
local shown = 0

local function distance(x, y, z, p)
    local dx, dy, dz = x - p[1], y - p[2], z - p[3]
    return math.sqrt(dx * dx + dy * dy + dz * dz)
end

-- 0 at `far` and beyond, 1 at `near` and closer.
local function closeness(d, near, far)
    return math.clamp((far - d) / (far - near), 0, 1)
end

local function free()
    player = nil
    stop_tone()
    set_name(IDLE_NAME)
    set_glow(0)
    shown = 0
end

function on_start()
    sense_hands(REACH)
    free()
    set_timer_event(0.25)
end

function on_hand(id, name, x, y, z, precise)
    if player == nil then
        player = id
        set_name("Theremin — " .. name .. " is playing")
    end
    if id ~= player then
        return
    end
    last_seen = get_time()

    -- Exponential in distance, so equal hand movements are equal musical
    -- intervals: that is what makes a theremin learnable.
    local t = closeness(distance(x, y, z, ROD), PITCH_NEAR, PITCH_FAR)
    local pitch = LOW * (HIGH / LOW) ^ t
    local gain = 1 - closeness(distance(x, y, z, LOOP), VOLUME_NEAR, VOLUME_FAR)
    -- A camera standing in for a hand (a developer at a desktop) is a toy;
    -- quieter, so nobody mistakes it for the real thing.
    if not precise then
        gain = gain * 0.5
    end
    set_tone{ voice = "sine", pitch = pitch, gain = gain, glide = 0.08, vibrato = 0.15, range = 25 }
    local glow = math.floor(gain * 8 + 0.5) / 10
    if glow ~= shown then
        set_glow(glow)
        shown = glow
    end
end

function on_timer()
    if player ~= nil and get_time() - last_seen > GONE_AFTER then
        free()
    end
end
