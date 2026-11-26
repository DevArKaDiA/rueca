CREATE TABLE `shop.sales.orders` (
  order_id INT64,
  placed_at TIMESTAMP,
  items ARRAY<STRUCT<sku STRING, qty INT64>>
);

CREATE VIEW `shop.sales.daily` AS
SELECT DATE(placed_at) AS day, COUNT(*) AS n
FROM `shop.sales.orders`
GROUP BY day;
