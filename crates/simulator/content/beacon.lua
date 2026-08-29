-- The beacon on the Grote Markt: a panel you press to call the firebird in, and
-- press again to send it back out over the city.
--
-- It is the worked example for **script-to-script messaging**, which is the one
-- thing a button needs and the `vf` API had no answer for: every call a script
-- can make acts on its own prim (`prim_id` is captured by the host, never passed
-- in), so a button cannot reach the thing it controls without a channel between
-- them. `vfSend` and `on_message` are that channel; `firebird.lua` beside this is
-- on the other end of it.
--
-- The panel holds no state of its own about where the firebird is. It asks, and
-- the firebird answers — because the firebird is the only thing that knows, and a
-- button that remembered would be wrong the moment anything else moved it (or the
-- firebird was reinstalled, or it went off and reformed).

-- The channel the pair talk on. Any number; it is private to scripts, so nobody
-- in-world hears it.
local CHANNEL = 4747

-- What the panel is showing. Set from the firebird's replies, never guessed.
local status = "?"

local function label()
    if status == "here" then
        return "Beacon — press to release the firebird"
    elseif status == "patrolling" then
        return "Beacon — press to summon the firebird"
    end
    return "Beacon — press to call the firebird"
end

-- The flame on top of it. A beacon has to be findable from across a square, and
-- the first version was not: a 1.1 m panel painted (0.10, 0.13, 0.16) is a dark
-- slab on grey stone, and the person it was built for could not find it. Size and
-- colour fixed that; the fire is what makes it visible from the far side.
local MARKER = {
    count = 70,
    life = 1.6,
    speed = 1.1,
    spread_deg = 16,
    gravity = 1.6,
    source_radius = 0.35,
    size_start = 0.55,
    size_end = 0.08,
    color_start = { 4.0, 1.6, 0.3 },
    color_end = { 0.9, 0.15, 0.03 },
    alpha = 0.8,
    turbulence = 0.7,
}

function on_start()
    vfSetName(label())
    -- Warm orange rather than near-black, and full bright so it keeps that colour
    -- at every time of day.
    vfSetColor(1.0, 0.42, 0.08)
    vfSetGlow(0.35)
    vfSetFullBright(true)
    -- A panel: wide, tall, thin, and tall enough to read as a thing somebody put
    -- there. It stands on the ground, so half its height up.
    vfSetScale(1.6, 2.6, 0.28)
    local x, _, z = vfGetPos()
    vfSetPos(x, 1.3, z)
    vfParticles(MARKER)
    vfListen(CHANNEL)
    -- Ask who is out there, so the label is right before anybody presses
    -- anything. A reply may never come — the firebird may not be installed — and
    -- the label above says the neutral thing until one does.
    vfSend(CHANNEL, "where")
end

function on_touch(avatar_id, avatar_name)
    -- The press is a *toggle*, and the panel deliberately does not decide which
    -- way. It says "somebody pressed me" and the firebird, which knows what it is
    -- doing, does the other thing. Two objects each holding half a state machine
    -- is how they come apart.
    vfSend(CHANNEL, "toggle " .. avatar_name)
    vfSay(avatar_name .. " presses the beacon.")
    -- A press should look like a press even before anything answers.
    vfSetGlow(1.0)
    vfSleep(0.25)
    vfSetGlow(0.35)
end

function on_message(channel, text, sender)
    if text == "here" or text == "patrolling" then
        status = text
        vfSetName(label())
        -- Hotter while the firebird is with you, cooler while it is away — the
        -- panel is the readout as well as the switch. Both stay bright enough to
        -- find; the state is in the hue, not in whether you can see it.
        if status == "here" then
            vfSetColor(1.0, 0.72, 0.30)
            vfSetGlow(0.7)
        else
            vfSetColor(1.0, 0.42, 0.08)
            vfSetGlow(0.35)
        end
    end
end
