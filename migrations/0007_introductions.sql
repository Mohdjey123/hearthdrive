-- Devices introduced by a paired device (its id): the main node introduces
-- the devices it is paired with to each other, so they pair without
-- scanning. Such a device is trusted no further than the device that
-- introduced it, and is forgotten when that device stops vouching for it.
ALTER TABLE devices ADD COLUMN introduced_by TEXT;
CREATE INDEX idx_devices_introduced_by ON devices (introduced_by);
