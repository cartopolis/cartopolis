-- The script API dropped its `vf` prefix (2026-10-04, protocol 33):
-- `vfSay` is `say`, `vfSetTimerEvent` is `set_timer_event`, and so on, with no
-- aliases. Rewrite every stored script so the world keeps running.
--
-- One statement per name, longest first: a single nested `replace` over all
-- 33 overflows SQLite's parser stack. No name occurs inside another, so the
-- order is belt and braces.
UPDATE prim_scripts SET source = replace(source, 'vfParticleBurst', 'particle_burst') WHERE instr(source, 'vfParticleBurst') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetFullBright', 'set_full_bright') WHERE instr(source, 'vfSetFullBright') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetTimerEvent', 'set_timer_event') WHERE instr(source, 'vfSetTimerEvent') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetPrimId', 'get_prim_id') WHERE instr(source, 'vfGetPrimId') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfParticles', 'particles') WHERE instr(source, 'vfParticles') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfResetTime', 'reset_time') WHERE instr(source, 'vfResetTime') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGeoScale', 'geo_scale') WHERE instr(source, 'vfGeoScale') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetAlpha', 'get_alpha') WHERE instr(source, 'vfGetAlpha') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetColor', 'get_color') WHERE instr(source, 'vfGetColor') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetScale', 'get_scale') WHERE instr(source, 'vfGetScale') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetAlpha', 'set_alpha') WHERE instr(source, 'vfSetAlpha') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetColor', 'set_color') WHERE instr(source, 'vfSetColor') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetScale', 'set_scale') WHERE instr(source, 'vfSetScale') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetGlow', 'get_glow') WHERE instr(source, 'vfGetGlow') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetName', 'get_name') WHERE instr(source, 'vfGetName') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetTime', 'get_time') WHERE instr(source, 'vfGetTime') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetGlow', 'set_glow') WHERE instr(source, 'vfSetGlow') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetName', 'set_name') WHERE instr(source, 'vfSetName') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfWhisper', 'whisper') WHERE instr(source, 'vfWhisper') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetGeo', 'get_geo') WHERE instr(source, 'vfGetGeo') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetPos', 'get_pos') WHERE instr(source, 'vfGetPos') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfGetRot', 'get_rot') WHERE instr(source, 'vfGetRot') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfListen', 'listen') WHERE instr(source, 'vfListen') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSensor', 'sensor') WHERE instr(source, 'vfSensor') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetGeo', 'set_geo') WHERE instr(source, 'vfSetGeo') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetPos', 'set_pos') WHERE instr(source, 'vfSetPos') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSetRot', 'set_rot') WHERE instr(source, 'vfSetRot') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfShout', 'shout') WHERE instr(source, 'vfShout') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSleep', 'sleep') WHERE instr(source, 'vfSleep') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSend', 'send') WHERE instr(source, 'vfSend') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfDie', 'die') WHERE instr(source, 'vfDie') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfLog', 'log') WHERE instr(source, 'vfLog') > 0;
UPDATE prim_scripts SET source = replace(source, 'vfSay', 'say') WHERE instr(source, 'vfSay') > 0;
-- Never an API name: the Grote Markt firework launchers were written to look for
-- a `vfPlaySound` before one existed and fall silent without it. Pointing them at
-- the real one is what makes them bang.
UPDATE prim_scripts SET source = replace(source, 'vfPlaySound', 'play_sound') WHERE instr(source, 'vfPlaySound') > 0;
