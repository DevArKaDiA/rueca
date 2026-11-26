CREATE VIEW customer_orders AS
SELECT c.email, o.id AS order_id, order_total(o.id) AS total
FROM customers c
JOIN orders o ON o.customer_id = c.id;
