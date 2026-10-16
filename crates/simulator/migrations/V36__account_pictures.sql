-- A person's picture: the face their disc shows in every list, call and map
-- marker instead of their initial (docs/notes/profile-pictures.md).
--
-- A hash in the picture store (`/api/pictures`), a small square JPEG that is
-- not sealed, exactly as a community's picture is (V32): it is shown to anybody
-- signed in who sees the name. NULL is "no picture", and the initial it was.
ALTER TABLE accounts ADD COLUMN picture TEXT;
