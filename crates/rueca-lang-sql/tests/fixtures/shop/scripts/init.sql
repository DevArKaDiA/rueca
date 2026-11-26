\set ON_ERROR_STOP on
\ir ../db/views.sql
SELECT count(*) FROM customer_orders;
