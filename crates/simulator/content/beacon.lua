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

function on_start()
    vfSetName(label())
    vfSetColor(0.10, 0.13, 0.16)
    vfSetGlow(0.35)
    vfSetFullBright(true)
    -- A panel: wide, tall, thin. It stands on the ground, so half its height up.
    vfSetScale(1.1, 1.5, 0.16)
    local x, _, z = vfGetPos()
    vfSetPos(x, 0.75, z)
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
        -- Lit while it is with you, dim while it is away — the panel is the
        -- readout as well as the switch.
        vfSetColor(0.10, 0.13, 0.16)
        if status == "here" then
            vfSetColor(0.35, 0.16, 0.05)
        end
    end
end
