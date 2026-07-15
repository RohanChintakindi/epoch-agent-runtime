ALTER TABLE epochs ADD COLUMN capability_frontier INTEGER NOT NULL DEFAULT 0
    CHECK (capability_frontier >= 0);
