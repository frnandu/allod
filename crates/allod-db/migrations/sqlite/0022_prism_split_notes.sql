-- A note the owner writes on a split for themselves: what or who the destination is.
-- Never sent anywhere; unknown to the payer and not copied to payouts.
ALTER TABLE prism_splits ADD COLUMN note TEXT;
