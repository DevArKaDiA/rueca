CREATE SCHEMA billing;

CREATE TABLE billing.invoice (
    id serial PRIMARY KEY,
    customer_id integer
);

CREATE TABLE public.customer (
    id serial PRIMARY KEY
);

CREATE VIEW billing.open_invoices AS
SELECT i.id, c.id AS customer
FROM billing.invoice i
JOIN customer c ON c.id = i.customer_id;
