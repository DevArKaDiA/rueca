-- name: GetCustomer :one
SELECT id, name, email FROM customers WHERE id = $1;

-- name: ListOrders :many
SELECT o.id, o.status FROM orders o WHERE o.customer_id = $1;
