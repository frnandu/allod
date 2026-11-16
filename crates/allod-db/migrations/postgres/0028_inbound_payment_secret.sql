ALTER TABLE transactions ADD COLUMN payment_secret TEXT;
CREATE INDEX tx_inbound_payment_secret
    ON transactions (payment_hash, payment_secret)
    WHERE direction = 'incoming';
