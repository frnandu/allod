-- A plain `user@domain` does not say whether it is a BIP-353 name or an LNURL address:
-- only asking does. Rows saved before that check call every plain name LNURL; mark them
-- unknown so the UI offers both, and saving them again records the real kind.
UPDATE prism_splits SET kind = 'address' WHERE kind = 'lnurl' AND destination LIKE '%@%';
