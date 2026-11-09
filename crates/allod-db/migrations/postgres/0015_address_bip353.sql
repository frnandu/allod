-- Whether an address publishes a BIP-353 record (SPEC §12.1); off: LNURL only.
ALTER TABLE ln_addresses ADD COLUMN bip353 BOOLEAN NOT NULL DEFAULT TRUE;
