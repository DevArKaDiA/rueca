CREATE TABLE payment (
    id integer,
    amount numeric
);

CREATE TABLE payment_2024 (
    CHECK (id > 0)
) INHERITS (payment);

CREATE MATERIALIZED VIEW payment_totals AS
SELECT sum(amount) AS total FROM payment;
