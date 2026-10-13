-- A public post's polaroid: a snapshot of the world at the post's place,
-- framed as a polaroid on the device that wrote it, beside the post's photo
-- (docs/notes/fediverse.md). A hash in the picture store, like `picture`, and
-- public like it.
ALTER TABLE posts ADD COLUMN snapshot TEXT;
