-- Where an account's avatar stood when its last session ended, so the next
-- sign-in — on any device — puts it back there (`db::last_place`,
-- `ServerHelloAck::last_place`). NULL until the account has walked somewhere
-- with a map under it; a call guest's session never writes it.
ALTER TABLE accounts ADD COLUMN last_lat REAL;
ALTER TABLE accounts ADD COLUMN last_lng REAL;
