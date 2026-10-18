-- How a person's avatar looks (protocol 42, docs/notes/makehuman-avatars.md):
-- the client's MakeHuman look as postcard bytes, about 150 of them. The
-- server checks only that it is reasonable (`AvatarLook::is_reasonable`) and
-- hands it to everyone who can see the avatar. NULL is "never set", and the
-- client draws its default body.
ALTER TABLE accounts ADD COLUMN look BLOB;
