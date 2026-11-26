select id, name from {{ source('shop', 'raw_customers') }}
