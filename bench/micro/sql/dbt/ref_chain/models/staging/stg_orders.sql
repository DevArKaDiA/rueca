select id as order_id, customer_id
from {{ source('shop', 'raw_orders') }}
