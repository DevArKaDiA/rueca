CREATE FUNCTION sales.net(gross FLOAT64) AS (gross * 0.8);

CREATE TABLE sales.events (
  user_id STRING,
  amount FLOAT64,
  at TIMESTAMP
);

CREATE VIEW sales.latest AS
SELECT user_id, sales.net(amount) AS net
FROM sales.events
WHERE TRUE
QUALIFY ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY at DESC) = 1;
