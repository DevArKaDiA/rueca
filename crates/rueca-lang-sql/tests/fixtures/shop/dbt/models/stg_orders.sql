select id as order_id, customer_id, status
from {{ source('app', 'orders') }}
where placed_at >= '{{ var("start_date") }}'
