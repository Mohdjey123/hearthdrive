-- Phase 6: which storage locations a paired device may see.
-- NULL means every location; otherwise a JSON array of storage root ids.
ALTER TABLE devices ADD COLUMN allowed_roots TEXT;
