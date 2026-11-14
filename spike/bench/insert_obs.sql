\set r random(1, 1000000000)
WITH o AS (
  INSERT INTO observations (id,scope,subject_id,place_id,property,value,class,pseudonym,observed_at,submitted_at)
  VALUES (fake_ulid('ob_'),'menu_item','mi_' || :r,'pl_x','price.amount','{"amount":30,"currency":"INR"}',
          'member', md5(:r::text)::varchar(22), now() - interval '1 minute', now())
  RETURNING id)
INSERT INTO changes (entity_type,entity_id,op) SELECT 'observation', id, 'insert' FROM o;
