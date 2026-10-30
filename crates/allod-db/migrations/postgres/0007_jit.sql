-- Just-in-time receive (SPEC §7.4): marks invoices that an LSPS2 LSP will pay into a
-- channel it opens on the first payment. The flag lets the wallet attribute a failed
-- channel opening to the invoice it was serving.
ALTER TABLE transactions ADD COLUMN jit BOOLEAN NOT NULL DEFAULT FALSE;
