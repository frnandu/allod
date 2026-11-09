-- Closed accounts give their Lightning address username back (SPEC §12).
UPDATE apps SET lud16_username = NULL WHERE closed_at IS NOT NULL;
