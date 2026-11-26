-- name: GetUser :one
SELECT id, name FROM users WHERE id = ?;

-- name: ListPosts :many
SELECT p.id FROM posts p JOIN users u ON u.id = p.user_id;
