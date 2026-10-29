-- Encryption of the app's latest NWC request (SPEC §8.5): notifications also go out as
-- kind 23196 (NIP-04) when it is 'nip04' or NULL (never used).
ALTER TABLE apps ADD COLUMN last_encryption TEXT;
