-- M5: the change feed (`WHERE seq > $1 ORDER BY seq LIMIT $2`) as an index-only range scan.
-- The index covers every column the feed returns; frequent insert-triggered autovacuum
-- keeps the visibility map current so heap fetches stay near zero.
CREATE INDEX changes_feed ON public.changes (seq) INCLUDE (entity_type, entity_id, op, at);
ALTER TABLE public.changes SET (autovacuum_vacuum_insert_scale_factor = 0.05);
