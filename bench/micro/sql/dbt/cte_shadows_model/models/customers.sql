select id from {{ source('shop', 'raw_customers') }}
