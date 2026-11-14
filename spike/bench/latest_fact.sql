\set n random(1, 5000)
SELECT o.* FROM bench_subjects s
JOIN LATERAL (SELECT value, class, pseudonym, observed_at FROM observations
              WHERE subject_id = s.subject_id AND property = s.property
              ORDER BY observed_at DESC LIMIT 20) o ON true
WHERE s.n = :n;
