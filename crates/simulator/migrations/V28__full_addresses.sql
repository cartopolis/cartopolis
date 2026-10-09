-- Protocol 37: every person in an encrypted chat is a full address
-- (`name@server`), inside each device's MLS credential as well as on the wire,
-- and a chat's id begins with its home server (`cartopolis_core::address`).
--
-- A device's credential cannot be changed, so devices made before this hold a
-- bare username for as long as they exist; the clients refuse their saved
-- state and start new devices. Their chats cannot carry on either: a group of
-- old-format members is a group nobody can join any more. So every private
-- chat is cleared rather than left as sealed bytes no client can read. Decided
-- 2026-10-09: the one live chat was a test between an operator and the bot.
DELETE FROM e2ee_invites;
DELETE FROM e2ee_log;
DELETE FROM e2ee_members;
DELETE FROM e2ee_groups;
DELETE FROM e2ee_key_packages;
DELETE FROM e2ee_devices;
