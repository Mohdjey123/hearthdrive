-- Encrypted backups: the backup's key, protected for this computer's
-- account (DPAPI on Windows). NULL for a backup that is not encrypted.
ALTER TABLE snapshot_sets ADD COLUMN key_sealed BLOB;
