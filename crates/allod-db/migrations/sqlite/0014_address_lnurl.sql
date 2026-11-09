-- Whether an address on a served domain also answers LNURL (SPEC §12.1); off: BIP-353 only.
ALTER TABLE ln_addresses ADD COLUMN lnurl BOOLEAN NOT NULL DEFAULT TRUE;
